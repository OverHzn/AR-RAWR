//! Differential tests for `db/finding_verification.go` — validation +
//! `SetFindingVerification`. Runs against `ARTEX_PG_DSN`; skips when unreachable.

use artex::db::finding_evidence::ERR_FINDING_NOT_FOUND;
use artex::db::finding_verification::{valid_verification_status, VERIFICATION_VERIFIED};

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

#[test]
fn valid_verification_status_cases() {
    for s in [
        "verified",
        "unverified",
        "blocked",
        "not_assessed",
        "false_positive",
    ] {
        assert!(valid_verification_status(s), "{s}");
    }
    for s in ["", "confirmed", "pending", "VERIFIED", "unknown"] {
        assert!(!valid_verification_status(s), "{s}");
    }
}

#[tokio::test]
async fn set_finding_verification_updates_only_three_columns() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let fid = d
        .add_finding(0, 0, "vc", "name", "high", "sum", "ev", "w", &[])
        .await
        .unwrap();
    d.execute(
        "UPDATE findings SET status='confirmed', report='body', evidence='orig' WHERE id=$1",
        &[&fid],
    )
    .await
    .unwrap();

    d.set_finding_verification(fid, VERIFICATION_VERIFIED, 3, "checked")
        .await
        .unwrap();
    let row = d
        .query_one(
            "SELECT verification_status, poe_level, verification_note, status, report, evidence FROM findings WHERE id=$1",
            &[&fid],
        )
        .await
        .unwrap();
    let status: String = row.get(0);
    let poe: i16 = row.get(1);
    let note: String = row.get(2);
    let triage: String = row.get(3);
    let report: String = row.get(4);
    let evidence: String = row.get(5);
    assert_eq!(status, "verified");
    assert_eq!(poe, 3);
    assert_eq!(note, "checked");
    assert_eq!(triage, "confirmed", "triage status untouched");
    assert_eq!(report, "body", "report untouched");
    assert_eq!(evidence, "orig", "evidence untouched");

    // Invalid status / POE rejected.
    assert!(d
        .set_finding_verification(fid, "bogus", 2, "n")
        .await
        .is_err());
    assert!(d
        .set_finding_verification(fid, "verified", 5, "n")
        .await
        .is_err());
    assert!(d
        .set_finding_verification(fid, "verified", -1, "n")
        .await
        .is_err());
    // Missing finding -> sentinel.
    let err = d
        .set_finding_verification(99_999_999, "verified", 1, "n")
        .await
        .expect_err("missing finding");
    assert!(err.is(ERR_FINDING_NOT_FOUND), "got {err}");

    let _ = d.delete_finding(fid).await;
}
