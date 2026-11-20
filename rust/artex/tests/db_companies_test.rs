//! Differential tests for the company entity layer — fresh spec for the pure-SQL
//! company methods in `db/companies.go` (UpsertCompany / GetCompany /
//! GetCompanyByName / UpsertByName / DeleteCompany / DeleteCompanyWithAssets /
//! ListCompanies / GetScope / RecomputeAttribution / MalformedIPAssetWarning /
//! ResolveCompany / ResolveCompanyWithICP). Scope rules are inserted with raw
//! SQL because the scope *parser* (AddScope/UpdateScope) is a separate later unit.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up every company/asset it creates.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::companies::{company_n_key, CompanyStore};
use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos + (N.fetch_add(1, Ordering::Relaxed) as u128) * 7919
}

async fn open(dsn: &str) -> Option<DB> {
    match DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

async fn cleanup_company(d: &DB, cs: &CompanyStore, id: i64) {
    let _ = d
        .execute("DELETE FROM company_scope WHERE company_id=$1", &[&id])
        .await;
    let _ = cs.delete_company(id).await;
}

#[tokio::test]
async fn company_entity_layer() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let cs = d.companies();
    let stamp = uniq();

    // ---- companyNKey parity: lower + collapse whitespace.
    assert_eq!(company_n_key("  Foo   BAR  "), "foo bar");

    // ---- UpsertCompany: first reports created=true, dedup by nkey -> false.
    let name = format!("Test Corp {stamp}");
    let (id, created) = cs
        .upsert_company(&name, "https://example.com/logo.png")
        .await
        .expect("upsert");
    assert!(created, "first upsert created=true");
    let (id2, created2) = cs.upsert_company(&name, "").await.expect("upsert dup");
    assert_eq!(id, id2, "dedup by nkey returns same id");
    assert!(!created2, "second upsert created=false");

    // ---- UpsertCompany keeps the original logo when the new one is empty
    // (COALESCE(EXCLUDED.logo, companies.logo)).
    let got = cs.get_company(id).await.expect("get").expect("exists");
    assert_eq!(got.name, name);
    assert_eq!(got.logo.as_deref(), Some("https://example.com/logo.png"));
    assert_eq!(got.nkey, company_n_key(&name));

    // ---- GetCompanyByName is normalised (different case/spacing).
    let by_name = cs
        .get_company_by_name(&name.to_uppercase())
        .await
        .expect("by name")
        .expect("found");
    assert_eq!(by_name.id, id);

    // ---- UpsertByName returns the id.
    let by_upsert = cs.upsert_by_name(&name).await.expect("upsert by name");
    assert_eq!(by_upsert, id);

    // ---- GetCompany: missing -> None.
    assert!(cs.get_company(99_999_999).await.expect("missing").is_none());
    assert!(cs
        .get_company_by_name("nope not here")
        .await
        .expect("missing name")
        .is_none());

    // ---- GetScope + ListCompanies + scope cascade on delete.
    d.execute(
        "INSERT INTO company_scope(company_id, kind, domain, raw) \
     VALUES ($1, 'domain', $2, $3)",
        &[
            &id,
            &format!("scope-{stamp}.example"),
            &format!("scope-{stamp}.example"),
        ],
    )
    .await
    .expect("insert domain scope");
    let scope = cs.get_scope(id).await.expect("scope");
    assert_eq!(scope.len(), 1);
    assert_eq!(scope[0].kind, "domain");
    assert_eq!(scope[0].domain, format!("scope-{stamp}.example"));
    assert_eq!(scope[0].company_id, id);

    let list = cs.list_companies().await.expect("list");
    let mine = list.iter().find(|c| c.company.id == id).expect("in list");
    assert_eq!(mine.scope.len(), 1, "list carries scope");
    assert_eq!(mine.asset_count, 0, "no assets yet");

    // delete_company cascades company_scope.
    cs.delete_company(id).await.expect("delete");
    assert!(cs.get_company(id).await.expect("gone").is_none());
    assert!(cs.get_scope(id).await.expect("scope gone").is_empty());

    // ---- DeleteCompanyWithAssets: deletes the company's assets, returns count.
    let stamp2 = uniq();
    let id_assets = cs
        .upsert_by_name(&format!("Delete Assets Company {stamp2}"))
        .await
        .expect("company for assets");
    let asset_id = d
        .query_one(
            "INSERT INTO assets(type, domain, root_domain, company_id, company_source) \
         VALUES ('root_domain', $1, $1, $2, 'explicit') RETURNING id",
            &[&format!("delete-assets-{stamp2}.example"), &id_assets],
        )
        .await
        .expect("insert owned asset")
        .get::<_, i64>(0);
    let deleted = cs
        .delete_company_with_assets(id_assets, true)
        .await
        .expect("delete with assets");
    assert_eq!(deleted, 1, "one asset deleted");
    assert!(cs.get_company(id_assets).await.expect("gone").is_none());
    let remaining = d
        .query_one("SELECT COUNT(*) FROM assets WHERE id=$1", &[&asset_id])
        .await
        .expect("count asset")
        .get::<_, i64>(0);
    assert_eq!(remaining, 0, "asset removed with company");

    // ---- DeleteCompanyWithAssets on a missing company -> ErrCompanyNotFound.
    let err = cs
        .delete_company_with_assets(99_999_999, false)
        .await
        .expect_err("missing -> error");
    assert!(err.is(artex::db::companies::ERR_COMPANY_NOT_FOUND));

    // ---- ResolveCompany / ResolveCompanyWithICP (domain / ip-cidr / icp).
    let rc = cs
        .upsert_by_name(&format!("ResolveCorp {stamp}"))
        .await
        .expect("resolve corp");
    let domain = format!("resolve-{stamp}.io");
    let cidr = "10.20.0.0/16";
    let icp_value = "myicp123"; // NormalizeICP("MY  ICP 123")
    for (kind, col, val) in [
        ("domain", "domain", domain.as_str()),
        ("cidr", "net", cidr),
        ("icp", "value", icp_value),
    ] {
        // domain/icp store text in their column; cidr stores a cidr literal.
        if kind == "cidr" {
            d.execute(
                "INSERT INTO company_scope(company_id, kind, net, raw) \
                 VALUES ($1, 'cidr', '10.20.0.0/16'::cidr, $2)",
                &[&rc, &format!("{kind} {val}")],
            )
            .await
            .expect("insert cidr scope");
        } else {
            d.execute(
                &format!(
                    "INSERT INTO company_scope(company_id, kind, {col}, raw) \
                 VALUES ($1, $2, $3, $4)"
                ),
                &[&rc, &kind, &val, &format!("{kind} {val}")],
            )
            .await
            .expect("insert scope");
        }
    }

    assert_eq!(
        cs.resolve_company(&domain, "")
            .await
            .expect("resolve domain"),
        Some(rc),
        "domain match"
    );
    assert_eq!(
        cs.resolve_company("notinscope.example", "")
            .await
            .expect("no match"),
        None,
        "no domain match"
    );
    assert_eq!(
        cs.resolve_company("", "10.20.5.1")
            .await
            .expect("cidr match"),
        Some(rc),
        "cidr match"
    );
    assert_eq!(
        cs.resolve_company("", "10.30.0.1")
            .await
            .expect("cidr no match"),
        None,
        "cidr outside range"
    );
    // ICP is normalised (case + whitespace removed) before matching.
    assert_eq!(
        cs.resolve_company_with_icp("", "", "MY  ICP 123")
            .await
            .expect("icp match"),
        Some(rc),
        "icp normalised match"
    );
    assert_eq!(
        cs.resolve_company_with_icp("", "", "other-icp")
            .await
            .expect("icp no match"),
        None,
        "icp no match"
    );

    cleanup_company(&d, &cs, rc).await;

    // ---- RecomputeAttribution: derived (company_source='scope') ownership.
    let ra = cs
        .upsert_by_name(&format!("AttributeCorp {stamp}"))
        .await
        .expect("attr corp");
    let attr_domain = format!("attr-{stamp}.com");
    let attr_cidr = "192.168.50.0/24";
    d.execute(
        "INSERT INTO company_scope(company_id, kind, domain, raw) VALUES ($1,'domain',$2,$2)",
        &[&ra, &attr_domain],
    )
    .await
    .expect("attr domain scope");
    d.execute(
        "INSERT INTO company_scope(company_id, kind, net, raw) \
     VALUES ($1,'cidr','192.168.50.0/24'::cidr,$2)",
        &[&ra, &attr_cidr],
    )
    .await
    .expect("attr cidr scope");

    // A root_domain asset and an ip asset, both unowned + explicit before recompute.
    let dom_asset = d
        .query_one(
            "INSERT INTO assets(type, domain, root_domain) VALUES ('root_domain',$1,$1) RETURNING id",
            &[&attr_domain],
        )
        .await
        .expect("dom asset")
        .get::<_, i64>(0);
    let ip_asset = d
        .query_one(
            "INSERT INTO assets(type, ip) VALUES ('ip', $1) RETURNING id",
            &[&"192.168.50.7"],
        )
        .await
        .expect("ip asset")
        .get::<_, i64>(0);
    // Before recompute, neither is attributed.
    let pre = d
        .query_one("SELECT company_id FROM assets WHERE id=$1", &[&dom_asset])
        .await
        .expect("pre")
        .get::<_, Option<i64>>(0);
    assert!(pre.is_none(), "no company before recompute");

    cs.recompute_attribution().await.expect("recompute");
    let dom_co = d
        .query_one("SELECT company_id FROM assets WHERE id=$1", &[&dom_asset])
        .await
        .expect("post")
        .get::<_, Option<i64>>(0);
    assert_eq!(dom_co, Some(ra), "domain-attributed to company");
    let ip_co = d
        .query_one("SELECT company_id FROM assets WHERE id=$1", &[&ip_asset])
        .await
        .expect("post ip")
        .get::<_, Option<i64>>(0);
    assert_eq!(ip_co, Some(ra), "cidr-attributed to company");

    cleanup_company(&d, &cs, ra).await;
    let _ = d
        .execute(
            "DELETE FROM assets WHERE id=ANY($1::bigint[])",
            &[&vec![dom_asset, ip_asset]],
        )
        .await;

    // ---- MalformedIPAssetWarning: a bad ip row is reported.
    let bad_asset = d
        .query_one(
            "INSERT INTO assets(type, ip) VALUES ('ip', $1) RETURNING id",
            &[&"not-an-ip-###"],
        )
        .await
        .expect("bad asset")
        .get::<_, i64>(0);
    let warning = cs.malformed_ip_asset_warning().await.expect("warning");
    assert!(
        warning.contains(&format!("#{bad_asset}")),
        "warning names the bad asset: {warning}"
    );
    assert!(
        warning.contains("not a valid IP"),
        "warning explains the problem: {warning}"
    );
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&bad_asset])
        .await;
}
