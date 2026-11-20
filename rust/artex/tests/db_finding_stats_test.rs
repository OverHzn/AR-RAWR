//! Differential tests for `FindingStats` in `db/findings.go` — the whole-table
//! aggregate including the archived-task fold (`archivedTaskAggregates`).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Assertions use deltas so pre-existing rows/archives don't
//! make the test brittle.

use artex::db::findings::FindingStats;

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

const DELTA_FIELDS: &str = "total/pending/submitted/accepted/critical/high/medium/low";

fn assert_delta(before: &FindingStats, after: &FindingStats, label: &str, d: [i64; 8]) {
    let got = [
        after.total - before.total,
        after.pending - before.pending,
        after.submitted - before.submitted,
        after.accepted - before.accepted,
        after.critical - before.critical,
        after.high - before.high,
        after.medium - before.medium,
        after.low - before.low,
    ];
    assert_eq!(got, d, "{label}: delta mismatch ({DELTA_FIELDS})");
}

#[tokio::test]
async fn finding_stats_live_and_archived() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let before = d.finding_stats().await.expect("baseline stats");

    // Seed a task with 3 findings of known severities/statuses/report stages.
    let t = d.create_task("stats", "goal", None, 0, 0).await.unwrap();
    let mut fids = Vec::new();
    for (i, (sev, status, rstate)) in [
        ("critical", "pending", "submitted"),
        ("high", "resolved", "accepted"),
        ("low", "ignored", "none"),
    ]
    .iter()
    .enumerate()
    {
        let node_id = d
            .query_one(
                "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
                 VALUES ($1,'finding','{}'::jsonb,9,'confirmed','tester') RETURNING id",
                &[&t.exploration_id],
            )
            .await
            .unwrap()
            .get::<_, i64>(0);
        let fid = d
            .add_finding(
                t.id,
                node_id,
                &format!("stat-class-{i}"),
                "n",
                sev,
                "s",
                "e",
                "w",
                &[],
            )
            .await
            .unwrap();
        d.execute(
            "UPDATE findings SET status=$2, report_state=$3 WHERE id=$1",
            &[&fid, status, rstate],
        )
        .await
        .unwrap();
        fids.push(fid);
    }

    let after = d.finding_stats().await.expect("after stats");
    // +3 total; pending +1; submitted +1; accepted +1; critical +1; high +1; low +1.
    assert_delta(&before, &after, "live", [3, 1, 1, 1, 1, 1, 0, 1]);
    assert!(
        after.vulnclasses.contains(&"stat-class-0".to_string())
            && after.vulnclasses.contains(&"stat-class-1".to_string())
            && after.vulnclasses.contains(&"stat-class-2".to_string()),
        "vulnclass filter set includes our classes"
    );
    let opt = after.tasks.iter().find(|o| o.id == t.id);
    assert!(opt.is_some(), "task dropdown includes our task");
    assert_eq!(opt.unwrap().count, 3, "task dropdown count = 3");

    // Seed a COLD archive: an archived task + an aggregate_stats JSON with a known
    // finding_stats section. FindingStats must fold it into counts + vulnclasses.
    let at = d.create_task("archived", "goal", None, 0, 0).await.unwrap();
    d.execute("UPDATE tasks SET archived_at=now() WHERE id=$1", &[&at.id])
        .await
        .unwrap();
    let agg = serde_json::json!({
        "finding_stats": {
            "total": 10, "pending": 2, "submitted": 3, "accepted": 4,
            "critical": 1, "high": 2, "medium": 3, "low": 4,
            "vulnclasses": ["archived-class", ""],
            "tasks": []
        }
    });
    d.execute(
        "INSERT INTO task_archives(task_id, state, aggregate_stats) VALUES ($1,'ready',$2::jsonb)",
        &[&at.id, &agg],
    )
    .await
    .unwrap();

    let with_archive = d.finding_stats().await.expect("archived stats");
    assert_delta(
        &after,
        &with_archive,
        "archive fold",
        [10, 2, 3, 4, 1, 2, 3, 4],
    );
    assert!(
        with_archive
            .vulnclasses
            .contains(&"archived-class".to_string()),
        "archived vulnclass merged"
    );
    assert!(
        !with_archive.vulnclasses.iter().any(|v| v.is_empty()),
        "empty vulnclass excluded"
    );
    // The archived task is NOT folded into the dropdown (Go leaves Tasks alone).
    assert!(
        !with_archive.tasks.iter().any(|o| o.id == at.id),
        "archived task not added to dropdown"
    );

    // Cleanup: archive row first, then tasks + their finding nodes.
    let _ = d
        .execute("DELETE FROM task_archives WHERE task_id=$1", &[&at.id])
        .await;
    let _ = d
        .execute("DELETE FROM findings WHERE task_id=$1", &[&t.id])
        .await;
    let _ = d
        .execute(
            "DELETE FROM exploration_nodes WHERE exploration_id=$1 AND kind='finding'",
            &[&t.exploration_id],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
    d.delete_task(at.id).await.unwrap();
}
