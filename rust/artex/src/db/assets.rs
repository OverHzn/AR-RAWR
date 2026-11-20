//! AssetStore foundation — slice of `db/assets.go` (root domain / IP writes,
//! reads, and task unlink/delete). The lock+tx protocol for company-scope
//! mutation is simplified here: company resolution runs on the pool before the
//! upsert statement. Advanced Upsert* variants remain deferred.

use std::time::SystemTime;

use serde_json::Value as Json;

use crate::db::asset_helpers::{
    calc_c_segment, marshal_jsonb_array, marshal_port_services, marshal_string_array,
    validate_asset_ip, PortService,
};
use crate::db::error::{Error, Result};
use crate::db::nkey::domain_key;
use crate::db::DB;

/// Minimal `Asset` row (the columns these methods read).
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub id: i64,
    pub typ: String,
    pub company_id: Option<i64>,
    pub task_ids: Vec<i64>,
    pub domain: String,
    pub root_domain: String,
    pub ip: String,
    pub c_segment: Option<String>,
    pub port: Option<i32>,
    pub icp: Option<String>,
    pub bound_domains: Vec<String>,
    pub open_ports: Vec<Json>,
    pub url: String,
    pub app_name: String,
    pub created_at: SystemTime,
}

const ASSET_COLS: &str = "id,type,company_id,task_ids,domain,root_domain,ip,c_segment::text,port,icp,bound_domains,open_ports,COALESCE(url,''),COALESCE(app_name,''),created_at";

fn scan_asset(row: &tokio_postgres::Row) -> Asset {
    Asset {
        id: row.get(0),
        typ: row.get(1),
        company_id: row.get(2),
        task_ids: row.get(3),
        domain: row.get::<_, Option<String>>(4).unwrap_or_default(),
        root_domain: row.get::<_, Option<String>>(5).unwrap_or_default(),
        ip: row.get::<_, Option<String>>(6).unwrap_or_default(),
        c_segment: row.get(7),
        port: row.get(8),
        icp: row.get(9),
        bound_domains: row.get(10),
        open_ports: row.get(11),
        url: row.get(12),
        app_name: row.get(13),
        created_at: row.get(14),
    }
}

#[derive(Clone)]
pub struct AssetStore {
    db: DB,
}

impl DB {
    /// `Assets`: the asset store handle (cheap clone of the pool).
    pub fn assets(&self) -> AssetStore {
        AssetStore { db: self.clone() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UpsertRootDomainReq {
    pub domain: String,
    pub icp: String,
    pub task_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UpsertIPReq {
    pub ip: String,
    pub bound_domains: Vec<String>,
    pub open_ports: Vec<PortService>,
    pub task_id: i64,
}

fn task_ids_array(task_id: i64) -> Vec<i64> {
    if task_id > 0 {
        vec![task_id]
    } else {
        Vec::new()
    }
}

impl AssetStore {
    /// `UpsertRootDomain`: idempotent insert/merge on the unique root_domain index.
    pub async fn upsert_root_domain(&self, req: &UpsertRootDomainReq) -> Result<i64> {
        let domain = domain_key(&req.domain);
        if domain.is_empty() {
            return Err(Error::msg("domain is required"));
        }
        let company = self
            .db
            .companies()
            .resolve_company_with_icp(&domain, "", &req.icp)
            .await?;
        let icp: Option<&str> = if req.icp.is_empty() {
            None
        } else {
            Some(&req.icp)
        };
        let tasks = task_ids_array(req.task_id);
        let row = self.db.query_one(
            "INSERT INTO assets(type, domain, root_domain, icp, company_id, company_source, task_ids)
VALUES ('root_domain', $1, $1, $2, $3, 'scope', $4::bigint[])
ON CONFLICT (domain) WHERE type = 'root_domain' DO UPDATE SET
    icp        = COALESCE(EXCLUDED.icp, assets.icp),
    company_id = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    task_ids   = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    extra      = assets.extra || EXCLUDED.extra,
    last_seen  = now()
RETURNING id",
            &[&domain, &icp, &company, &tasks],
        ).await?;
        Ok(row.get(0))
    }

    /// `UpsertIP`: idempotent insert/merge with C segment, bound domains and
    /// de-duplicated open ports. Validates that `ip` is an address, not a host.
    pub async fn upsert_ip(&self, req: &UpsertIPReq) -> Result<i64> {
        if req.ip.is_empty() {
            return Err(Error::msg("ip is required"));
        }
        validate_asset_ip(&req.ip)?;
        let cseg = calc_c_segment(&req.ip);
        let cseg_opt: Option<&str> = if cseg.is_empty() { None } else { Some(&cseg) };
        let company = self
            .db
            .companies()
            .resolve_company_with_icp("", &req.ip, "")
            .await?;
        let tasks = task_ids_array(req.task_id);
        let bound = marshal_string_array(&req.bound_domains);
        let ports = marshal_port_services(&req.open_ports)?;
        // Go binds the array literal as text[] / jsonb[]; cast through ::text[]
        // so the same escaped literal is parsed by Postgres.
        let row = self.db.query_one(
            "INSERT INTO assets(type, ip, c_segment, bound_domains, open_ports, company_id, company_source, task_ids)
VALUES ('ip', $1, $2::text::cidr, $3::text::text[], $4::text::jsonb[], $5, 'scope', $6::bigint[])
ON CONFLICT (ip) WHERE type = 'ip' DO UPDATE SET
    bound_domains = (SELECT ARRAY(SELECT DISTINCT unnest(assets.bound_domains || EXCLUDED.bound_domains))),
    open_ports    = (
        SELECT ARRAY(
            SELECT DISTINCT ON ((elem->>'port')::int) elem
            FROM unnest(assets.open_ports || EXCLUDED.open_ports) AS elem
            ORDER BY (elem->>'port')::int,
                     CASE WHEN elem->>'service' IS NOT NULL AND elem->>'service' <> '' THEN 0 ELSE 1 END
        )
    ),
    c_segment  = COALESCE(assets.c_segment, EXCLUDED.c_segment),
    company_id = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    task_ids   = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    last_seen  = now()
RETURNING id",
            &[&req.ip, &cseg_opt, &bound, &ports, &company, &tasks],
        ).await?;
        Ok(row.get(0))
    }

    /// `AppendIPPort`: append one {port, service} entry to an existing IP asset.
    pub async fn append_ip_port(&self, ip: &str, port: i32, service: &str) -> Result<()> {
        if ip.is_empty() || port == 0 {
            return Ok(());
        }
        let entry = serde_json::json!({ "port": port, "service": service });
        let lit = marshal_jsonb_array(&[entry])?;
        self.db.execute(
            "UPDATE assets SET
    open_ports = (
        SELECT ARRAY(
            SELECT DISTINCT ON ((elem->>'port')::int) elem
            FROM unnest(open_ports || $2::text::jsonb[]) AS elem
            ORDER BY (elem->>'port')::int,
                     CASE WHEN elem->>'service' IS NOT NULL AND elem->>'service' <> '' THEN 0 ELSE 1 END
        )
    ), last_seen = now()
WHERE type='ip' AND ip=$1",
            &[&ip, &lit],
        ).await?;
        Ok(())
    }

    pub async fn query_by_type(&self, typ: &str, limit: i64, offset: i64) -> Result<Vec<Asset>> {
        let rows = self
            .db
            .query(
                &format!(
                    "SELECT {ASSET_COLS} FROM assets WHERE type=$1 ORDER BY id LIMIT $2 OFFSET $3"
                ),
                &[&typ, &limit, &offset],
            )
            .await?;
        Ok(rows.iter().map(scan_asset).collect())
    }

    pub async fn get_by_ids(&self, ids: &[i64]) -> Result<Vec<Asset>> {
        let rows = self
            .db
            .query(
                &format!("SELECT {ASSET_COLS} FROM assets WHERE id = ANY($1) ORDER BY id"),
                &[&ids],
            )
            .await?;
        Ok(rows.iter().map(scan_asset).collect())
    }

    pub async fn delete_by_ids(&self, ids: &[i64]) -> Result<u64> {
        self.db
            .execute("DELETE FROM assets WHERE id = ANY($1)", &[&ids])
            .await
    }

    /// `HostsByTask`: distinct domains of assets linked to a task (plus IP hosts).
    pub async fn hosts_by_task(&self, task_id: i64) -> Result<Vec<String>> {
        let rows = self.db.query(
            "SELECT DISTINCT COALESCE(NULLIF(domain,''), NULLIF(ip,'')) FROM assets WHERE $1=ANY(task_ids) AND COALESCE(domain,'')<>'' OR ($1=ANY(task_ids) AND COALESCE(ip,'')<>'')",
            &[&task_id],
        ).await?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// `DeleteByTaskID`: delete assets exclusive to the task; shared assets are
    /// only unlinked. Returns the number of rows deleted.
    pub async fn delete_by_task_id(&self, task_id: i64) -> Result<u64> {
        self.db
            .execute(
                "DELETE FROM assets WHERE $1=ANY(task_ids) AND cardinality(task_ids)=1",
                &[&task_id],
            )
            .await?;
        self.db
            .execute(
                "UPDATE assets SET task_ids = array_remove(task_ids, $1) WHERE $1=ANY(task_ids)",
                &[&task_id],
            )
            .await
    }
}
