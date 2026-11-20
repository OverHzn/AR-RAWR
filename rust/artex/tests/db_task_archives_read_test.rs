//! Differential tests for the `db/task_archives.go` read slice — format gate
//! (port of TestTaskArchiveFormatCompatibility) + blockers (TestTaskArchiveBlockers…)
//! + archive record reads. Runs against `ARTEX_PG_DSN`.

use artex::db::task_archives::{
    is_task_archive_format_supported, ARCHIVE_QUEUED, TASK_ARCHIVE_FORMAT_VERSION,
    TASK_ARCHIVE_LEGACY_FORMAT_VERSION,
};
use artex::db::tasks::TaskCreateOptions;

#[test]
fn format_compatibility() {
    for v in [
        TASK_ARCHIVE_LEGACY_FORMAT_VERSION,
        TASK_ARCHIVE_FORMAT_VERSION,
        2,
    ] {
        assert!(is_task_archive_format_supported(v), "version {v}");
    }
    for v in [0, -1, TASK_ARCHIVE_FORMAT_VERSION + 1] {
        assert!(!is_task_archive_format_supported(v), "version {v}");
    }
}

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

#[tokio::test]
async fn blockers_ignore_queued_dependents_and_reads() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let source = d
        .create_task("archive-src", "source", None, 0, 0)
        .await
        .unwrap();
    let dep = d
        .create_task_with_options(
            "archive-dep",
            "dep",
            TaskCreateOptions {
                source_task_ids: vec![source.id],
                ..TaskCreateOptions::default()
            },
        )
        .await
        .unwrap();
    let _ = d
        .execute(
            "DELETE FROM task_archives WHERE task_id IN ($1,$2)",
            &[&source.id, &dep.id],
        )
        .await;

    // Live dependent blocks source.
    let blockers = d.task_archive_blockers().await.unwrap();
    assert_eq!(
        blockers.get(&source.id),
        Some(&dep.id),
        "dependent blocks source"
    );
    assert_eq!(d.task_archive_blocker(source.id).await.unwrap(), dep.id);
    assert_eq!(d.task_archive_blocker(dep.id).await.unwrap(), 0);

    // Seed a queued archive for the dependent -> no longer blocks source.
    d.execute(
        "INSERT INTO task_archives(task_id, state, phase, format_version) VALUES ($1,$2,'queued',$3)",
        &[&dep.id, &ARCHIVE_QUEUED, &TASK_ARCHIVE_FORMAT_VERSION],
    ).await.unwrap();
    let blockers = d.task_archive_blockers().await.unwrap();
    assert_eq!(
        blockers.get(&source.id),
        None,
        "queued dependent must not block"
    );
    assert_eq!(d.task_archive_blocker(source.id).await.unwrap(), 0);

    // Archive record reads.
    let rec = d.get_task_archive_by_task(dep.id).await.unwrap().unwrap();
    assert_eq!(rec.task_id, dep.id);
    assert_eq!(rec.state, ARCHIVE_QUEUED);
    assert_eq!(rec.format_version, TASK_ARCHIVE_FORMAT_VERSION);
    assert!(d.get_task_archive(rec.id).await.unwrap().is_some());
    assert!(d.get_task_archive(99_999_999).await.unwrap().is_none());

    let page = d.list_task_archives("", "", 1, 20).await.unwrap();
    assert!(page.items.iter().any(|x| x.id == rec.id));
    assert!(page.total >= 1);

    let _ = d
        .execute(
            "DELETE FROM task_archives WHERE task_id IN ($1,$2)",
            &[&source.id, &dep.id],
        )
        .await;
    let _ = d.delete_task(dep.id).await;
    let _ = d.delete_task(source.id).await;
}
