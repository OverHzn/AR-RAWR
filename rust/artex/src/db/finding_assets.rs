//! Finding asset tree — port of `db/finding_assets.go`.
//!
//! The tree is a one-shot snapshot: findings are counted onto one deduplicated
//! ancestor set each, and the same tree resolves an asset-scope node key into the
//! asset ids of its whole subtree (no truncation in that mode).

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::findings::{FindingFilter, FINDING_UNASSIGNED_ASSET};
use crate::db::DB;

const FINDING_ASSET_TREE_MAX_NODES: i32 = 5000;

#[derive(Debug, Clone, PartialEq)]
pub struct FindingAssetNode {
    pub key: String,
    pub parent: String,
    pub kind: String,
    pub label: String,
    pub asset_id: i64,
    pub company_id: i64,
    pub self_count: i64,
    pub total: i64,
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub last_found_at: SystemTime,
}

impl Default for FindingAssetNode {
    fn default() -> Self {
        FindingAssetNode {
            key: String::new(),
            parent: String::new(),
            kind: String::new(),
            label: String::new(),
            asset_id: 0,
            company_id: 0,
            self_count: 0,
            total: 0,
            critical: 0,
            high: 0,
            medium: 0,
            low: 0,
            last_found_at: SystemTime::UNIX_EPOCH,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindingAssetTree {
    pub nodes: Vec<FindingAssetNode>,
    pub finding_total: i64,
    pub truncated: bool,
    pub dropped_kinds: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct AssetRow {
    id: i64,
    kind: String,
    company_id: i64,
    domain: String,
    root_domain: String,
    ip: String,
    url: String,
    port: i32,
    service_type: String,
    app_name: String,
}

#[derive(Debug, Clone)]
struct FindingAssetHit {
    severity: String,
    ts: SystemTime,
    asset_ids: Vec<i64>,
}

impl Default for FindingAssetHit {
    fn default() -> Self {
        FindingAssetHit {
            severity: String::new(),
            ts: SystemTime::UNIX_EPOCH,
            asset_ids: Vec::new(),
        }
    }
}

const ASSET_SELECT_COLS: &str = "a.id, a.type, COALESCE(a.company_id,0), COALESCE(a.domain,''), COALESCE(a.root_domain,''), COALESCE(a.ip,''), COALESCE(a.url,''), COALESCE(a.port,0), COALESCE(a.service_type,''), COALESCE(a.app_name,'')";

fn scan_asset_rows(rows: &[tokio_postgres::Row]) -> Vec<AssetRow> {
    rows.iter()
        .map(|r| AssetRow {
            id: r.get(0),
            kind: r.get(1),
            company_id: r.get(2),
            domain: r.get(3),
            root_domain: r.get(4),
            ip: r.get(5),
            url: r.get(6),
            port: r.get(7),
            service_type: r.get(8),
            app_name: r.get(9),
        })
        .collect()
}

/// `hostPort`: domain first, then URL host, then IP.
fn host_port(a: &AssetRow) -> (String, i32) {
    let mut host = a.domain.clone();
    let mut port = a.port;
    if host.is_empty() && !a.url.is_empty() {
        if let Ok(u) = url::Url::parse(&a.url) {
            host = u.host_str().unwrap_or("").to_lowercase();
            if port == 0 {
                port = u
                    .port()
                    .map(|p| p as i32)
                    .unwrap_or_else(|| match u.scheme() {
                        "http" | "ws" => 80,
                        "https" | "wss" => 443,
                        "ftp" => 21,
                        _ => 0,
                    });
            }
        }
    }
    if host.is_empty() {
        host = a.ip.clone();
    }
    (host, port)
}

/// `label`: URL > domain > ip > app_name > root_domain; services without a URL
/// get the port appended so they don't duplicate their host row's label.
fn asset_label(a: &AssetRow) -> String {
    if a.kind == "service" && a.url.is_empty() {
        let (host, port) = host_port(a);
        if !host.is_empty() && port > 0 {
            return format!("{host}:{port}");
        }
    }
    for v in [&a.url, &a.domain, &a.ip, &a.app_name, &a.root_domain] {
        if !v.is_empty() {
            return v.clone();
        }
    }
    format!("a:{}", a.id)
}

fn asset_key(id: i64) -> String {
    format!("a:{id}")
}
fn company_key(id: i64) -> String {
    format!("c:{id}")
}

fn is_ip_literal(host: &str) -> bool {
    if host.contains(':') {
        return true;
    }
    if host.is_empty() {
        return false;
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.parse::<i64>().is_ok())
}

fn count_finding(n: &mut FindingAssetNode, h: &FindingAssetHit) {
    n.total += 1;
    match h.severity.as_str() {
        "critical" => n.critical += 1,
        "high" => n.high += 1,
        "medium" => n.medium += 1,
        "low" => n.low += 1,
        _ => {}
    }
    if h.ts > n.last_found_at {
        n.last_found_at = h.ts;
    }
}

#[derive(Debug, Clone, Default)]
struct MissingHosts {
    services: Vec<String>,
    domains: Vec<String>,
    ips: Vec<String>,
    roots: Vec<String>,
}

impl MissingHosts {
    fn empty(&self) -> bool {
        self.services.is_empty()
            && self.domains.is_empty()
            && self.ips.is_empty()
            && self.roots.is_empty()
    }
}

fn keys_of(set: &HashSet<String>) -> Vec<String> {
    set.iter().cloned().collect()
}

fn missing_parents(by_id: &HashMap<i64, AssetRow>) -> MissingHosts {
    let mut have_service = HashSet::new();
    let mut have_domain = HashSet::new();
    let mut have_ip = HashSet::new();
    let mut have_root = HashSet::new();
    for a in by_id.values() {
        match a.kind.as_str() {
            "service" => {
                let (h, _) = host_port(a);
                if !h.is_empty() {
                    have_service.insert(h);
                }
            }
            "subdomain" => {
                have_domain.insert(a.domain.clone());
            }
            "ip" => {
                have_ip.insert(a.ip.clone());
            }
            "root_domain" => {
                have_root.insert(a.domain.clone());
            }
            _ => {}
        }
    }
    let mut want_service = HashSet::new();
    let mut want_domain = HashSet::new();
    let mut want_ip = HashSet::new();
    let mut want_root = HashSet::new();
    for a in by_id.values() {
        match a.kind.as_str() {
            "service" | "endpoint" => {
                let (host, _) = host_port(a);
                if a.kind == "endpoint" && !host.is_empty() && !have_service.contains(&host) {
                    want_service.insert(host.clone());
                }
                if !host.is_empty()
                    && !have_domain.contains(&host)
                    && !have_ip.contains(&host)
                    && !have_root.contains(&host)
                {
                    if is_ip_literal(&host) {
                        want_ip.insert(host.clone());
                    } else {
                        want_domain.insert(host.clone());
                    }
                }
                if !a.ip.is_empty() && !have_ip.contains(&a.ip) {
                    want_ip.insert(a.ip.clone());
                }
            }
            "subdomain" if !a.root_domain.is_empty() && !have_root.contains(&a.root_domain) => {
                want_root.insert(a.root_domain.clone());
            }
            _ => {}
        }
    }
    MissingHosts {
        services: keys_of(&want_service),
        domains: keys_of(&want_domain),
        ips: keys_of(&want_ip),
        roots: keys_of(&want_root),
    }
}

impl DB {
    /// `BuildFindingAssetTree`: capped tree snapshot.
    pub async fn build_finding_asset_tree(&self, f: &FindingFilter) -> Result<FindingAssetTree> {
        self.build_finding_asset_tree_capped(f, FINDING_ASSET_TREE_MAX_NODES)
            .await
    }

    /// `buildFindingAssetTree` with a node cap; `max_nodes<=0` = no truncation
    /// (required by `apply_asset_scope` so subtree ids stay complete).
    pub(crate) async fn build_finding_asset_tree_capped(
        &self,
        f: &FindingFilter,
        max_nodes: i32,
    ) -> Result<FindingAssetTree> {
        let mut filter = f.clone();
        filter.asset_scope = String::new();
        filter.asset_ids = Vec::new();
        filter.asset_none = false;
        filter.asset_miss = false;
        let (where_sql, params) = filter.build_where();
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_tosql()).collect();
        let rows = self.query(
            &format!("SELECT COALESCE(f.severity,''), f.created_at, COALESCE(f.asset_ids::text,'[]') \
                      FROM findings f LEFT JOIN tasks t ON f.task_id = t.id{where_sql}"),
            &refs,
        ).await?;
        let mut hits = Vec::with_capacity(rows.len());
        let mut asset_id_set = HashSet::new();
        for r in &rows {
            let aids: String = r.get(2);
            let ids: Vec<i64> = serde_json::from_str(&aids).unwrap_or_default();
            for id in &ids {
                if *id > 0 {
                    asset_id_set.insert(*id);
                }
            }
            hits.push(FindingAssetHit {
                severity: r.get(0),
                ts: r.get(1),
                asset_ids: ids,
            });
        }

        let mut tree = FindingAssetTree {
            finding_total: hits.len() as i64,
            ..FindingAssetTree::default()
        };
        let by_id = self.load_finding_asset_rows(&asset_id_set).await?;
        let (mut nodes, mut parent_of) = assemble_finding_asset_nodes(&by_id);
        attach_company_nodes(&mut nodes, &mut parent_of, self).await?;

        let mut unassigned = FindingAssetNode {
            key: FINDING_UNASSIGNED_ASSET.to_string(),
            kind: "none".to_string(),
            label: "Unlinked assets".to_string(),
            ..FindingAssetNode::default()
        };
        for h in &hits {
            let mut touched = HashSet::new();
            let mut direct = Vec::new();
            for id in &h.asset_ids {
                if let Some(node) = nodes.get(&asset_key(*id)) {
                    direct.push(node.key.clone());
                    let mut k = node.key.clone();
                    while !k.is_empty() {
                        touched.insert(k.clone());
                        k = parent_of.get(&k).cloned().unwrap_or_default();
                    }
                }
            }
            if direct.is_empty() {
                count_finding(&mut unassigned, h);
                unassigned.self_count += 1;
                continue;
            }
            for key in &direct {
                if let Some(n) = nodes.get_mut(key) {
                    n.self_count += 1;
                }
            }
            for key in &touched {
                if let Some(n) = nodes.get_mut(key) {
                    count_finding(n, h);
                }
            }
        }
        for node in nodes.into_values() {
            if node.total > 0 {
                tree.nodes.push(node);
            }
        }
        if unassigned.total > 0 {
            tree.nodes.push(unassigned);
        }
        sort_finding_asset_nodes(&mut tree.nodes);
        truncate_finding_asset_tree(&mut tree, max_nodes);
        Ok(tree)
    }

    async fn load_finding_asset_rows(&self, ids: &HashSet<i64>) -> Result<HashMap<i64, AssetRow>> {
        let mut by_id = HashMap::new();
        if ids.is_empty() {
            return Ok(by_id);
        }
        let id_list: Vec<i64> = ids.iter().copied().collect();
        let rows = self
            .query(
                &format!("SELECT {ASSET_SELECT_COLS} FROM assets a WHERE a.id = ANY($1::bigint[])"),
                &[&id_list],
            )
            .await?;
        for a in scan_asset_rows(&rows) {
            by_id.insert(a.id, a);
        }
        for _ in 0..4 {
            let want = missing_parents(&by_id);
            if want.empty() {
                break;
            }
            let added = self.load_assets_by_host(&want, &mut by_id).await?;
            if added == 0 {
                break;
            }
        }
        Ok(by_id)
    }

    async fn load_assets_by_host(
        &self,
        want: &MissingHosts,
        by_id: &mut HashMap<i64, AssetRow>,
    ) -> Result<i64> {
        let mut added = 0;
        let queries = [
            (format!("SELECT {ASSET_SELECT_COLS} FROM assets a WHERE a.type='service' AND (a.domain = ANY($1::text[]) OR a.ip = ANY($1::text[]))"), &want.services),
            (format!("SELECT {ASSET_SELECT_COLS} FROM assets a WHERE a.type='subdomain' AND a.domain = ANY($1::text[])"), &want.domains),
            (format!("SELECT {ASSET_SELECT_COLS} FROM assets a WHERE a.type='ip' AND a.ip = ANY($1::text[])"), &want.ips),
            (format!("SELECT {ASSET_SELECT_COLS} FROM assets a WHERE a.type='root_domain' AND a.domain = ANY($1::text[])"), &want.roots),
        ];
        for (q, arg) in queries {
            if arg.is_empty() {
                continue;
            }
            let rows = self.query(&q, &[&arg]).await?;
            for a in scan_asset_rows(&rows) {
                if by_id.contains_key(&a.id) {
                    continue;
                }
                by_id.insert(a.id, a);
                added += 1;
            }
        }
        Ok(added)
    }
}

/// Resolve asset rows into nodes + parent links (synthesizing missing roots).
fn assemble_finding_asset_nodes(
    by_id: &HashMap<i64, AssetRow>,
) -> (HashMap<String, FindingAssetNode>, HashMap<String, String>) {
    let mut nodes = HashMap::new();
    let mut parent_of = HashMap::new();
    let mut root_by_domain = HashMap::new();
    let mut sub_by_domain = HashMap::new();
    let mut ip_by_addr = HashMap::new();
    let mut svc_by_host = HashMap::new();
    let mut svc_by_host_port = HashMap::new();

    for a in by_id.values() {
        let key = asset_key(a.id);
        nodes.insert(
            key.clone(),
            FindingAssetNode {
                key: key.clone(),
                kind: a.kind.clone(),
                label: asset_label(a),
                asset_id: a.id,
                company_id: a.company_id,
                ..FindingAssetNode::default()
            },
        );
        match a.kind.as_str() {
            "root_domain" if !a.domain.is_empty() => {
                root_by_domain.insert(a.domain.clone(), key);
            }
            "subdomain" if !a.domain.is_empty() => {
                sub_by_domain.insert(a.domain.clone(), key);
            }
            "ip" if !a.ip.is_empty() => {
                ip_by_addr.insert(a.ip.clone(), key);
            }
            "service" => {
                let (host, port) = host_port(a);
                if !host.is_empty() {
                    svc_by_host.insert(host.clone(), key.clone());
                    svc_by_host_port.insert(format!("{host}|{port}"), key);
                }
            }
            _ => {}
        }
    }
    // Synthesize missing root-domain nodes for subdomains.
    for a in by_id.values() {
        if a.kind != "subdomain" || a.root_domain.is_empty() {
            continue;
        }
        if root_by_domain.contains_key(&a.root_domain) {
            continue;
        }
        let key = format!("r:{}", a.root_domain);
        nodes.insert(
            key.clone(),
            FindingAssetNode {
                key: key.clone(),
                kind: "root_domain".to_string(),
                label: a.root_domain.clone(),
                ..FindingAssetNode::default()
            },
        );
        root_by_domain.insert(a.root_domain.clone(), key);
    }
    fn first_of(nodes: &HashMap<String, FindingAssetNode>, keys: &[&str]) -> String {
        keys.iter()
            .find(|k| nodes.contains_key(**k))
            .map(|k| k.to_string())
            .unwrap_or_default()
    }
    for a in by_id.values() {
        let key = asset_key(a.id);
        let parent = match a.kind.as_str() {
            "subdomain" => first_of(
                &nodes,
                &[root_by_domain
                    .get(&a.root_domain)
                    .map(|x| x.as_str())
                    .unwrap_or("")],
            ),
            "service" => {
                let (host, _) = host_port(a);
                first_of(
                    &nodes,
                    &[
                        sub_by_domain
                            .get(&a.domain)
                            .map(|x| x.as_str())
                            .unwrap_or(""),
                        sub_by_domain.get(&host).map(|x| x.as_str()).unwrap_or(""),
                        ip_by_addr.get(&a.ip).map(|x| x.as_str()).unwrap_or(""),
                        ip_by_addr.get(&host).map(|x| x.as_str()).unwrap_or(""),
                        root_by_domain
                            .get(&a.root_domain)
                            .map(|x| x.as_str())
                            .unwrap_or(""),
                        root_by_domain.get(&host).map(|x| x.as_str()).unwrap_or(""),
                    ],
                )
            }
            "endpoint" => {
                let (host, port) = host_port(a);
                first_of(
                    &nodes,
                    &[
                        svc_by_host_port
                            .get(&format!("{host}|{port}"))
                            .map(|x| x.as_str())
                            .unwrap_or(""),
                        svc_by_host.get(&host).map(|x| x.as_str()).unwrap_or(""),
                        sub_by_domain.get(&host).map(|x| x.as_str()).unwrap_or(""),
                        sub_by_domain
                            .get(&a.domain)
                            .map(|x| x.as_str())
                            .unwrap_or(""),
                        ip_by_addr.get(&host).map(|x| x.as_str()).unwrap_or(""),
                        ip_by_addr.get(&a.ip).map(|x| x.as_str()).unwrap_or(""),
                        root_by_domain
                            .get(&a.root_domain)
                            .map(|x| x.as_str())
                            .unwrap_or(""),
                        root_by_domain.get(&host).map(|x| x.as_str()).unwrap_or(""),
                    ],
                )
            }
            _ => String::new(),
        };
        if !parent.is_empty() && parent != key {
            if let Some(n) = nodes.get_mut(&key) {
                n.parent = parent.clone();
            }
            parent_of.insert(key, parent);
        }
    }
    (nodes, parent_of)
}

async fn attach_company_nodes(
    nodes: &mut HashMap<String, FindingAssetNode>,
    parent_of: &mut HashMap<String, String>,
    db: &DB,
) -> Result<()> {
    let mut want = HashSet::new();
    for n in nodes.values() {
        if n.parent.is_empty()
            && n.company_id > 0
            && matches!(n.kind.as_str(), "root_domain" | "ip" | "app")
        {
            want.insert(n.company_id);
        }
    }
    if want.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = want.iter().copied().collect();
    let rows = db
        .query(
            "SELECT id, COALESCE(name,'') FROM companies WHERE id = ANY($1::bigint[])",
            &[&ids],
        )
        .await?;
    let mut names = HashMap::new();
    for r in &rows {
        names.insert(r.get::<_, i64>(0), r.get::<_, String>(1));
    }
    for (id, name) in names {
        let key = company_key(id);
        if nodes.contains_key(&key) {
            continue;
        }
        let label = if name.is_empty() {
            format!("Company #{id}")
        } else {
            name
        };
        nodes.insert(
            key.clone(),
            FindingAssetNode {
                key: key.clone(),
                kind: "company".to_string(),
                label,
                company_id: id,
                ..FindingAssetNode::default()
            },
        );
    }
    let updates: Vec<(String, String, String)> = nodes
        .values()
        .filter(|n| {
            n.parent.is_empty()
                && n.company_id > 0
                && n.kind != "company"
                && matches!(n.kind.as_str(), "root_domain" | "ip" | "app")
        })
        .filter_map(|n| {
            let k = company_key(n.company_id);
            if nodes.contains_key(&k) {
                Some((n.key.clone(), k.clone(), k))
            } else {
                None
            }
        })
        .collect();
    for (node_key, parent, _company) in updates {
        if let Some(n) = nodes.get_mut(&node_key) {
            n.parent = parent.clone();
        }
        parent_of.insert(node_key, parent);
    }
    Ok(())
}

fn sort_finding_asset_nodes(nodes: &mut [FindingAssetNode]) {
    nodes.sort_by(|a, b| {
        if (a.kind == "none") != (b.kind == "none") {
            return if b.kind == "none" {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        b.total.cmp(&a.total).then_with(|| a.label.cmp(&b.label))
    });
}

fn truncate_finding_asset_tree(tree: &mut FindingAssetTree, max_nodes: i32) {
    if max_nodes <= 0 || tree.nodes.len() as i32 <= max_nodes {
        return;
    }
    for kind in ["endpoint", "service"] {
        tree.nodes.retain(|n| n.kind != kind);
        tree.truncated = true;
        tree.dropped_kinds.push(kind.to_string());
        if tree.nodes.len() as i32 <= max_nodes {
            return;
        }
    }
}
