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
use crate::db::nkey::{domain_key, root as nkey_root};
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

#[derive(Debug, Clone, Default)]
pub struct UpsertSubdomainReq {
    pub domain: String,
    pub record_type: String,
    pub record_value: Vec<String>,
    pub icp: String,
    pub task_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UpsertEndpointReq {
    pub url: String,
    pub method: String,
    pub params: Vec<Json>,
    /// Optional IP, from async DNS.
    pub ip: String,
    pub task_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UpsertOtherServiceReq {
    pub domain: String,
    pub ip: String,
    pub port: i32,
    pub service_name: String,
    pub auth: Vec<Json>,
    pub task_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UpsertHTTPServiceReq {
    pub url: String,
    pub technologies: Vec<String>,
    pub status_code: Option<i32>,
    pub content_length: Option<i64>,
    pub page_title: String,
    pub favicon_mmh3: String,
    /// JSON objects appended to the asset's auth array.
    pub auth: Vec<Json>,
    /// Optional IP, from async DNS.
    pub ip: String,
    pub task_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UpsertAppReq {
    pub name: String,
    pub bundle_id: String,
    pub category: String,
    pub description: String,
    pub icp: String,
    /// Explicit company override (None = auto-attribute by ICP).
    pub company_id: Option<i64>,
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

    /// `UpsertSubdomain`: idempotent subdomain insert/merge with side effects —
    /// ensures the root domain exists and binds A/AAAA record IPs.
    pub async fn upsert_subdomain(&self, req: &UpsertSubdomainReq) -> Result<i64> {
        let domain = domain_key(&req.domain);
        if domain.is_empty() {
            return Err(Error::msg("domain is required"));
        }
        let (root_domain, _) = nkey_root(&domain);
        let root_domain = if root_domain.is_empty() {
            domain.clone()
        } else {
            root_domain
        };
        let mut company = self
            .db
            .companies()
            .resolve_company_with_icp(&root_domain, "", &req.icp)
            .await?;

        // Side effect 1: ensure the root domain asset exists.
        let _ = self
            .upsert_root_domain(&UpsertRootDomainReq {
                domain: root_domain.clone(),
                task_id: req.task_id,
                ..Default::default()
            })
            .await;

        // Side effect 2: each A/AAAA record becomes an IP asset bound to the domain.
        let mut ip_str = String::new();
        if req.record_type == "A" || req.record_type == "AAAA" {
            for rv in &req.record_value {
                for part in rv.split(',') {
                    let candidate = part.trim();
                    if candidate.is_empty() || candidate.parse::<std::net::IpAddr>().is_err() {
                        continue;
                    }
                    if ip_str.is_empty() {
                        ip_str = candidate.to_string();
                    }
                    let _ = self
                        .upsert_ip(&UpsertIPReq {
                            ip: candidate.to_string(),
                            bound_domains: vec![domain.clone()],
                            task_id: req.task_id,
                            ..Default::default()
                        })
                        .await;
                }
            }
        }

        let cseg = calc_c_segment(&ip_str);
        let cseg_opt: Option<&str> = if cseg.is_empty() { None } else { Some(&cseg) };
        let ip_opt: Option<&str> = if ip_str.is_empty() {
            None
        } else {
            Some(&ip_str)
        };
        let icp_opt: Option<&str> = if req.icp.is_empty() {
            None
        } else {
            Some(&req.icp)
        };
        if company.is_none() && !ip_str.is_empty() {
            company = self
                .db
                .companies()
                .resolve_company_with_icp("", &ip_str, "")
                .await?;
        }
        let tasks = task_ids_array(req.task_id);
        let values = marshal_string_array(&req.record_value);
        let row = self.db.query_one(
            "INSERT INTO assets(type, domain, root_domain, record_type, record_value, ip, c_segment, icp, company_id, company_source, task_ids)
VALUES ('subdomain', $1, $2, $3, $4::text::text[], $5, $6::text::cidr, $7, $8, 'scope', $9::bigint[])
ON CONFLICT (domain, COALESCE(record_type,'')) WHERE type = 'subdomain' DO UPDATE SET
    ip           = COALESCE(EXCLUDED.ip, assets.ip),
    c_segment    = COALESCE(EXCLUDED.c_segment, assets.c_segment),
    icp          = COALESCE(EXCLUDED.icp, assets.icp),
    company_id   = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    task_ids     = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    record_value = (SELECT ARRAY(SELECT DISTINCT unnest(assets.record_value || EXCLUDED.record_value))),
    extra        = assets.extra || EXCLUDED.extra,
    last_seen    = now()
RETURNING id",
            &[&domain, &root_domain, &req.record_type, &values, &ip_opt, &cseg_opt, &icp_opt, &company, &tasks],
        ).await?;
        Ok(row.get(0))
    }

    /// `UpsertApp`: idempotent app insert/merge, keyed by bundle id when present
    /// (otherwise by exact app name). An explicit company id wins over scope.
    pub async fn upsert_app(&self, req: &UpsertAppReq) -> Result<i64> {
        if req.name.is_empty() {
            return Err(Error::msg("app name is required"));
        }
        let tasks = task_ids_array(req.task_id);
        let bundle: Option<&str> = if req.bundle_id.is_empty() {
            None
        } else {
            Some(&req.bundle_id)
        };
        let category: Option<&str> = if req.category.is_empty() {
            None
        } else {
            Some(&req.category)
        };
        let description: Option<&str> = if req.description.is_empty() {
            None
        } else {
            Some(&req.description)
        };
        let icp: Option<&str> = if req.icp.is_empty() {
            None
        } else {
            Some(&req.icp)
        };
        let (company, source) = if let Some(id) = req.company_id {
            (Some(id), "explicit")
        } else {
            (
                self.db
                    .companies()
                    .resolve_company_with_icp("", "", &req.icp)
                    .await?,
                "scope",
            )
        };

        let sql = if req.bundle_id.is_empty() {
            "INSERT INTO assets(type, bundle_id, app_name, category, app_description, app_icp, company_id, company_source, task_ids)
VALUES ('app', NULL, $1, $2, $3, $4, $5, $6, $7::bigint[])
ON CONFLICT (app_name) WHERE type = 'app' AND bundle_id IS NULL DO UPDATE SET
    category        = COALESCE(EXCLUDED.category, assets.category),
    app_description = COALESCE(EXCLUDED.app_description, assets.app_description),
    app_icp         = COALESCE(EXCLUDED.app_icp, assets.app_icp),
    company_id      = CASE
        WHEN EXCLUDED.company_source = 'explicit' THEN EXCLUDED.company_id
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source  = CASE
        WHEN EXCLUDED.company_source = 'explicit' THEN 'explicit'
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    task_ids        = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    last_seen       = now()
RETURNING id"
        } else {
            "INSERT INTO assets(type, bundle_id, app_name, category, app_description, app_icp, company_id, company_source, task_ids)
VALUES ('app', $1, $2, $3, $4, $5, $6, $7, $8::bigint[])
ON CONFLICT (bundle_id) WHERE type = 'app' AND bundle_id IS NOT NULL DO UPDATE SET
    app_name        = COALESCE(EXCLUDED.app_name, assets.app_name),
    category        = COALESCE(EXCLUDED.category, assets.category),
    app_description = COALESCE(EXCLUDED.app_description, assets.app_description),
    app_icp         = COALESCE(EXCLUDED.app_icp, assets.app_icp),
    company_id      = CASE
        WHEN EXCLUDED.company_source = 'explicit' THEN EXCLUDED.company_id
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source  = CASE
        WHEN EXCLUDED.company_source = 'explicit' THEN 'explicit'
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    task_ids        = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    last_seen       = now()
RETURNING id"
        };
        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = if req.bundle_id.is_empty() {
            vec![
                &req.name,
                &category,
                &description,
                &icp,
                &company,
                &source,
                &tasks,
            ]
        } else {
            vec![
                &bundle,
                &req.name,
                &category,
                &description,
                &icp,
                &company,
                &source,
                &tasks,
            ]
        };
        let row = self.db.query_one(sql, &params).await?;
        Ok(row.get(0))
    }

    /// `UpsertHTTPService`: insert/merge an HTTP service asset; domain, port,
    /// service name and root domain are derived from the normalized URL.
    pub async fn upsert_http_service(&self, req: &UpsertHTTPServiceReq) -> Result<i64> {
        if req.url.is_empty() {
            return Err(Error::msg("url is required"));
        }
        validate_asset_ip(&req.ip)?;
        let norm_url = crate::db::asset_helpers::normalize_url(&req.url);
        let (domain, port, service_name) = crate::db::asset_helpers::parse_url(&norm_url);
        let root_domain = {
            let (r, _) = nkey_root(&domain);
            if r.is_empty() {
                domain.clone()
            } else {
                r
            }
        };
        let cseg = calc_c_segment(&req.ip);
        let company = self
            .db
            .companies()
            .resolve_company_with_icp(&root_domain, &req.ip, "")
            .await?;
        let tasks = task_ids_array(req.task_id);
        let techs = marshal_string_array(&req.technologies);
        let auth = marshal_jsonb_array(&req.auth)?;
        let domain_opt: Option<&str> = if domain.is_empty() {
            None
        } else {
            Some(&domain)
        };
        let ip_opt: Option<&str> = if req.ip.is_empty() {
            None
        } else {
            Some(&req.ip)
        };
        let cseg_opt: Option<&str> = if cseg.is_empty() { None } else { Some(&cseg) };
        let title_opt: Option<&str> = if req.page_title.is_empty() {
            None
        } else {
            Some(&req.page_title)
        };
        let favicon_opt: Option<&str> = if req.favicon_mmh3.is_empty() {
            None
        } else {
            Some(&req.favicon_mmh3)
        };
        let port_opt = crate::db::asset_helpers::nullable_int(port);

        let row = self.db.query_one(
            "INSERT INTO assets(
    type, url, service_type, service_name, domain, ip, port, root_domain,
    c_segment, favicon_mmh3, technologies, status_code, content_length, page_title,
    auth, company_id, company_source, task_ids
)
VALUES (
    'service', $1, 'http', $2, $3, $4, $5, $6,
    $7::text::cidr, $8, $9::text::text[], $10, $11, $12,
    $13::text::jsonb[], $14, 'scope', $15::bigint[]
)
ON CONFLICT (url) WHERE type = 'service' AND service_type = 'http' DO UPDATE SET
    status_code    = COALESCE(EXCLUDED.status_code,    assets.status_code),
    content_length = COALESCE(EXCLUDED.content_length, assets.content_length),
    page_title     = COALESCE(EXCLUDED.page_title,     assets.page_title),
    favicon_mmh3   = COALESCE(EXCLUDED.favicon_mmh3,   assets.favicon_mmh3),
    ip             = COALESCE(EXCLUDED.ip,             assets.ip),
    c_segment      = COALESCE(EXCLUDED.c_segment,      assets.c_segment),
    technologies   = (SELECT ARRAY(SELECT DISTINCT unnest(assets.technologies || EXCLUDED.technologies))),
    auth           = (
        SELECT ARRAY(
            SELECT DISTINCT ON (elem::text) elem
            FROM unnest(assets.auth || EXCLUDED.auth) AS elem
        )
    ),
    task_ids       = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    company_id     = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    last_seen      = now()
RETURNING id",
            &[&norm_url, &service_name, &domain_opt, &ip_opt, &port_opt, &root_domain,
              &cseg_opt, &favicon_opt, &techs, &req.status_code, &req.content_length, &title_opt,
              &auth, &company, &tasks],
        ).await?;
        let id: i64 = row.get(0);

        // Side effects: register root domain + subdomain, then bind the IP.
        if !root_domain.is_empty() {
            let _ = self
                .upsert_root_domain(&UpsertRootDomainReq {
                    domain: root_domain.clone(),
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        if !domain.is_empty()
            && domain != root_domain
            && domain.parse::<std::net::IpAddr>().is_err()
        {
            let _ = self
                .upsert_subdomain(&UpsertSubdomainReq {
                    domain: domain.clone(),
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        if !req.ip.is_empty() {
            let bound = if domain.is_empty() {
                Vec::new()
            } else {
                vec![domain]
            };
            let ports = if port > 0 {
                vec![PortService {
                    port,
                    service: service_name,
                }]
            } else {
                Vec::new()
            };
            let _ = self
                .upsert_ip(&UpsertIPReq {
                    ip: req.ip.clone(),
                    bound_domains: bound,
                    open_ports: ports,
                    task_id: req.task_id,
                })
                .await;
        }
        Ok(id)
    }

    /// `UpsertOtherService`: idempotent non-HTTP service insert/merge keyed by
    /// (domain, ip, port, service_name), with auth array append and IP side effect.
    pub async fn upsert_other_service(&self, req: &UpsertOtherServiceReq) -> Result<i64> {
        if req.domain.is_empty() && req.ip.is_empty() {
            return Err(Error::msg("domain or ip is required"));
        }
        validate_asset_ip(&req.ip)?;
        if req.port == 0 {
            return Err(Error::msg("port is required"));
        }
        if req.service_name.is_empty() {
            return Err(Error::msg("service_name is required"));
        }
        let service_name = req.service_name.trim().to_lowercase();
        let domain = domain_key(&req.domain);
        let root_domain = if domain.is_empty() {
            String::new()
        } else {
            let (r, _) = nkey_root(&domain);
            if r.is_empty() {
                domain.clone()
            } else {
                r
            }
        };
        let cseg = calc_c_segment(&req.ip);
        let company = self
            .db
            .companies()
            .resolve_company_with_icp(&root_domain, &req.ip, "")
            .await?;
        let tasks = task_ids_array(req.task_id);
        let auth = marshal_jsonb_array(&req.auth)?;
        let domain_opt: Option<&str> = if domain.is_empty() {
            None
        } else {
            Some(&domain)
        };
        let root_opt: Option<&str> = if root_domain.is_empty() {
            None
        } else {
            Some(&root_domain)
        };
        let ip_opt: Option<&str> = if req.ip.is_empty() {
            None
        } else {
            Some(&req.ip)
        };
        let cseg_opt: Option<&str> = if cseg.is_empty() { None } else { Some(&cseg) };

        let row = self.db.query_one(
            "INSERT INTO assets(
    type, service_type, service_name, domain, ip, port, root_domain,
    c_segment, auth, company_id, company_source, task_ids
)
VALUES ('service', 'other', $1, $2, $3, $4, $5, $6::text::cidr, $7::text::jsonb[], $8, 'scope', $9::bigint[])
ON CONFLICT (COALESCE(domain,''), COALESCE(ip,''), port, service_name) WHERE type = 'service' AND service_type = 'other' DO UPDATE SET
    domain     = COALESCE(EXCLUDED.domain,     assets.domain),
    ip         = COALESCE(EXCLUDED.ip,         assets.ip),
    c_segment  = COALESCE(EXCLUDED.c_segment,  assets.c_segment),
    auth       = (
        SELECT ARRAY(
            SELECT DISTINCT ON (elem::text) elem
            FROM unnest(assets.auth || EXCLUDED.auth) AS elem
        )
    ),
    task_ids   = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    company_id = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    last_seen  = now()
RETURNING id",
            &[&service_name, &domain_opt, &ip_opt, &req.port, &root_opt, &cseg_opt, &auth, &company, &tasks],
        ).await?;
        let id: i64 = row.get(0);

        if !req.ip.is_empty() && req.port > 0 {
            let bound = if domain.is_empty() {
                Vec::new()
            } else {
                vec![domain.clone()]
            };
            let _ = self
                .upsert_ip(&UpsertIPReq {
                    ip: req.ip.clone(),
                    bound_domains: bound,
                    open_ports: vec![PortService {
                        port: req.port,
                        service: service_name,
                    }],
                    task_id: req.task_id,
                })
                .await;
        }
        if !root_domain.is_empty() {
            let _ = self
                .upsert_root_domain(&UpsertRootDomainReq {
                    domain: root_domain.clone(),
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        if !domain.is_empty()
            && domain != root_domain
            && domain.parse::<std::net::IpAddr>().is_err()
        {
            let _ = self
                .upsert_subdomain(&UpsertSubdomainReq {
                    domain,
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        Ok(id)
    }

    /// `UpsertEndpoint`: idempotent endpoint insert/merge keyed by (URL, method),
    /// with params merged by (location, name) and host side effects.
    pub async fn upsert_endpoint(&self, req: &UpsertEndpointReq) -> Result<i64> {
        if req.url.is_empty() {
            return Err(Error::msg("url is required"));
        }
        if req.method.is_empty() {
            return Err(Error::msg("method is required"));
        }
        validate_asset_ip(&req.ip)?;
        let method = req.method.to_uppercase();
        let norm_url = crate::db::asset_helpers::normalize_url(&req.url);
        let (domain, port, _) = crate::db::asset_helpers::parse_url(&norm_url);
        let root_domain = {
            let (r, _) = nkey_root(&domain);
            if r.is_empty() {
                domain.clone()
            } else {
                r
            }
        };
        let cseg = calc_c_segment(&req.ip);
        let company = self
            .db
            .companies()
            .resolve_company_with_icp(&root_domain, &req.ip, "")
            .await?;
        let tasks = task_ids_array(req.task_id);
        let params = marshal_jsonb_array(&req.params)?;
        let domain_opt: Option<&str> = if domain.is_empty() {
            None
        } else {
            Some(&domain)
        };
        let root_opt: Option<&str> = if root_domain.is_empty() {
            None
        } else {
            Some(&root_domain)
        };
        let ip_opt: Option<&str> = if req.ip.is_empty() {
            None
        } else {
            Some(&req.ip)
        };
        let cseg_opt: Option<&str> = if cseg.is_empty() { None } else { Some(&cseg) };
        let port_opt = crate::db::asset_helpers::nullable_int(port);

        let row = self.db.query_one(
            "INSERT INTO assets(
    type, url, method, domain, ip, port, root_domain,
    params, company_id, company_source, task_ids, c_segment
)
VALUES ('endpoint', $1, $2, $3, $4, $5, $6, $7::text::jsonb[], $8, 'scope', $9::bigint[], $10::text::cidr)
ON CONFLICT (url, method) WHERE type = 'endpoint' DO UPDATE SET
    params     = (
        SELECT ARRAY(
            SELECT DISTINCT ON ((elem->>'location'), (elem->>'name')) elem
            FROM unnest(assets.params || EXCLUDED.params) AS elem
            ORDER BY (elem->>'location'), (elem->>'name'), elem::text DESC
        )
    ),
    ip         = COALESCE(EXCLUDED.ip,        assets.ip),
    c_segment  = COALESCE(EXCLUDED.c_segment, assets.c_segment),
    task_ids   = (SELECT ARRAY(SELECT DISTINCT unnest(assets.task_ids || EXCLUDED.task_ids))),
    company_id = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN assets.company_id
        ELSE COALESCE(EXCLUDED.company_id, assets.company_id)
    END,
    company_source = CASE
        WHEN assets.company_source = 'explicit' AND assets.company_id IS NOT NULL THEN 'explicit'
        WHEN EXCLUDED.company_id IS NOT NULL THEN 'scope'
        ELSE assets.company_source
    END,
    last_seen  = now()
RETURNING id",
            &[&norm_url, &method, &domain_opt, &ip_opt, &port_opt, &root_opt,
              &params, &company, &tasks, &cseg_opt],
        ).await?;
        let id: i64 = row.get(0);

        if !root_domain.is_empty() {
            let _ = self
                .upsert_root_domain(&UpsertRootDomainReq {
                    domain: root_domain.clone(),
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        if !domain.is_empty()
            && domain != root_domain
            && domain.parse::<std::net::IpAddr>().is_err()
        {
            let _ = self
                .upsert_subdomain(&UpsertSubdomainReq {
                    domain: domain.clone(),
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        if !req.ip.is_empty() {
            let _ = self
                .upsert_ip(&UpsertIPReq {
                    ip: req.ip.clone(),
                    task_id: req.task_id,
                    ..Default::default()
                })
                .await;
        }
        Ok(id)
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
