//! Differential tests for the scope-write half of `db/companies.go` (D-3b) —
//! `CreateCompanyWithScope` / `AddScope*` / `UpdateScope*` + checked errors +
//! derived attribution. Port of the key cases in `company_scope_test.go` and
//! `company_scope_consistency_test.go`.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::companies::{ScopeWriteResult, ERR_COMPANY_NAME_CONFLICT, ERR_COMPANY_NOT_FOUND};
use artex::db::company_scope::ScopeInput;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<artex::db::DB> {
    match artex::db::DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

fn input(kind: &str, value: &str) -> ScopeInput {
    ScopeInput {
        kind: kind.to_string(),
        value: value.to_string(),
    }
}

#[tokio::test]
async fn create_company_with_scope_and_attribution() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let companies = d.companies();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let domain = format!("cscope-{stamp}.example");
    let name = format!("Create Scope {stamp}");

    // Create with valid domain + keyword + one invalid rule.
    let inputs = [
        input("domain", &domain),
        input("keyword", "acme"),
        input("bogus", "x"), // invalid kind
    ];
    let (id, res): (i64, ScopeWriteResult) = companies
        .create_company_with_scope(&name, "", &inputs, "test")
        .await
        .unwrap();
    assert!(id > 0);
    assert_eq!(res.added, 2, "domain + keyword stored");
    assert_eq!(res.invalid, 1, "bogus kind rejected");
    assert_eq!(res.validation_errors.len(), 1);

    // Scope rules readable (2).
    let scope = companies.get_scope(id).await.unwrap();
    assert_eq!(scope.len(), 2);

    // Duplicate name -> sentinel conflict.
    let err = companies
        .create_company_with_scope(&name, "", &[], "test")
        .await
        .expect_err("duplicate name");
    assert!(err.is(ERR_COMPANY_NAME_CONFLICT), "got {err}");

    // Seed an EXPLICITLY owned asset whose domain matches the new scope. An
    // explicit row (company_id set + source='explicit') must survive the rebuild.
    let (explicit_id, _) = companies
        .upsert_company(&format!("Explicit {stamp}"), "")
        .await
        .unwrap();
    let asset_id = d
        .query_one(
            "INSERT INTO assets(type, domain, root_domain, company_id, company_source)              VALUES ('root_domain',$1,$1,$2,'explicit') RETURNING id",
            &[&domain, &explicit_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    companies.recompute_attribution().await.unwrap();
    let arow = d
        .query_one(
            "SELECT company_id, company_source FROM assets WHERE id=$1",
            &[&asset_id],
        )
        .await
        .unwrap();
    let cid: Option<i64> = arow.get(0);
    let source: String = arow.get(1);
    assert_eq!(cid, Some(explicit_id), "explicit owner preserved");
    assert_eq!(source, "explicit", "explicit provenance preserved");

    // Cleanup.
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await;
    let _ = companies.delete_company(explicit_id).await;
    let _ = companies.delete_company(id).await;
}

#[tokio::test]
async fn add_update_scope_checked_and_bounds() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let companies = d.companies();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (id, _) = companies
        .upsert_company(&format!("Add Update {stamp}"), "")
        .await
        .unwrap();

    // AddScopeInputsChecked: 3 valid rules; duplicates skipped on a second add.
    let rules = [
        input("domain", &format!("a-{stamp}.example")),
        input("icp", &format!("ICP-{stamp}")),
        input("cidr", &format!("10.{}.0.0/16", stamp % 200)),
    ];
    let res = companies
        .add_scope_inputs_checked(id, &rules, "test")
        .await
        .unwrap();
    assert_eq!(res.added, 3);
    assert_eq!(res.invalid, 0);
    let res2 = companies
        .add_scope_inputs_checked(id, &rules, "test")
        .await
        .unwrap();
    assert_eq!(res2.added, 0, "duplicates ignored by ON CONFLICT");
    assert_eq!(res2.skipped, 3);

    // A mixed add reports invalid separately but still stores the valid one.
    let mixed = [
        input("domain", &format!("b-{stamp}.example")),
        input("bad", "x"),
    ];
    let res = companies
        .add_scope_inputs_checked(id, &mixed, "test")
        .await
        .unwrap();
    assert_eq!(res.added, 1);
    assert_eq!(res.invalid, 1);
    assert_eq!(res.validation_errors.len(), 1);

    // Missing company -> sentinel.
    let missing = id + 1_000_000_000;
    let err = companies
        .add_scope_inputs_checked(missing, &[], "test")
        .await
        .expect_err("missing company");
    assert!(err.is(ERR_COMPANY_NOT_FOUND), "got {err}");
    let err = companies
        .update_scope_inputs_checked(missing, &[], "test")
        .await
        .expect_err("missing company update");
    assert!(err.is(ERR_COMPANY_NOT_FOUND), "got {err}");

    // UpdateScopeInputsChecked replaces all; an invalid rule aborts the whole
    // replacement (existing scope untouched).
    let before = companies.get_scope(id).await.unwrap().len();
    let bad_update = [
        input("domain", &format!("c-{stamp}.example")),
        input("bad", "x"),
    ];
    assert!(companies
        .update_scope_inputs_checked(id, &bad_update, "test")
        .await
        .is_err());
    assert_eq!(
        companies.get_scope(id).await.unwrap().len(),
        before,
        "scope preserved"
    );

    let good_update = [input("domain", &format!("d-{stamp}.example"))];
    let res = companies
        .update_scope_inputs_checked(id, &good_update, "test")
        .await
        .unwrap();
    assert_eq!(res.added, 1);
    assert_eq!(
        companies.get_scope(id).await.unwrap().len(),
        1,
        "replaced with one rule"
    );

    // Rune bounds: exactly 1024 'é' accepted, 1025 rejected.
    let boundary = "\u{e9}".repeat(1024);
    let res = companies
        .add_scope_inputs_checked(id, &[input("keyword", &boundary)], "test")
        .await
        .unwrap();
    assert_eq!(res.added, 1, "1024-rune boundary accepted");
    let over = "\u{e9}".repeat(1025);
    assert!(
        companies
            .add_scope_inputs_checked(id, &[input("keyword", &over)], "test")
            .await
            .is_err(),
        "1025 runes rejected"
    );

    // Bulk add (300 keyword rules) — rule count is unbounded.
    let bulk: Vec<ScopeInput> = (0..300)
        .map(|i| input("keyword", &format!("bulk-{stamp}-{i}")))
        .collect();
    let res = companies
        .add_scope_inputs_checked(id, &bulk, "test")
        .await
        .unwrap();
    assert_eq!(res.added, 300);

    let _ = companies.delete_company(id).await;
}

#[tokio::test]
async fn add_scope_raw_lines_helper() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let companies = d.companies();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (id, _) = companies
        .upsert_company(&format!("Raw Lines {stamp}"), "")
        .await
        .unwrap();
    let lines = vec![
        format!("line-{stamp}.example"),
        "  ACME   Security  ".to_string(),
    ];
    let res = companies.add_scope(id, &lines, "test").await;
    assert_eq!(res.added, 2, "raw domain + keyword parsed and stored");
    assert_eq!(res.invalid, 0);
    let _ = companies.update_scope(id, &lines, "test").await;
    let _ = companies.delete_company(id).await;
}
