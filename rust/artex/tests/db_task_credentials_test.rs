//! Differential tests for the per-task credential ledger — fresh spec for
//! `db/task_credentials.go` (the Go package exercises this column only via
//! `secrets_test.go`, with no dedicated CRUD spec).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Passwords are sealed via the secret column, so the test
//! installs the process-wide secret box (mirroring the Go `TestMain`).

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::secrets::{set_active_secret_box, SecretBox};
use artex::db::task_credentials::{TaskCredentialInput, ERR_TASK_CREDENTIAL_INVALID};
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

/// Install the secret box under the test key (idempotent).
fn ensure_box() {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    let b = SecretBox::new(&key).expect("test key is 32 bytes");
    set_active_secret_box(b);
}

/// Seed a throwaway exploration + task via raw SQL; return the task id.
async fn seed_task(d: &DB, stamp: u128) -> i64 {
    let goal = format!("t-cred-goal-{stamp}");
    let exp: i64 = d
        .query_one(
            "INSERT INTO explorations(goal) VALUES ($1) RETURNING id",
            &[&goal],
        )
        .await
        .expect("insert exploration")
        .get(0);
    d.query_one(
        "INSERT INTO tasks(description, goal, exploration_id) VALUES ($1, $1, $2) RETURNING id",
        &[&goal, &exp],
    )
    .await
    .expect("insert task")
    .get(0)
}

#[tokio::test]
async fn task_credential_upsert_list_delete() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    ensure_box();
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let task_id = seed_task(&d, stamp).await;

    // Create (source=user, empty password allowed).
    let created = d
        .upsert_task_credential(
            task_id,
            TaskCredentialInput {
                domain: "Example.COM ".into(),
                username: " alice ".into(),
                password: String::new(),
                source: String::new(),
                note: " first ".into(),
            },
        )
        .await
        .expect("upsert create");
    assert_eq!(created.domain, "Example.COM", "domain trimmed (case kept)");
    assert_eq!(created.username, "alice", "username trimmed");
    assert_eq!(created.source, "user", "empty source defaults to user");
    assert_eq!(created.note, "first", "note trimmed");
    assert_eq!(created.password, "", "plaintext empty");
    assert_eq!(created.task_id, task_id);

    // Upsert same (task, domain, username) with a password → overwrite in place.
    let updated = d
        .upsert_task_credential(
            task_id,
            TaskCredentialInput {
                domain: "Example.COM".into(),
                username: "alice".into(),
                password: "s3cr3t".into(),
                source: "agent".into(),
                note: String::new(),
            },
        )
        .await
        .expect("upsert update");
    assert_eq!(updated.id, created.id, "same row updated, not a new one");
    assert_eq!(updated.password, "s3cr3t", "plaintext password returned");
    assert_eq!(updated.source, "agent");

    // List: password round-trips decrypted, oldest-first.
    let list = d.task_credentials(task_id).await.expect("list");
    assert_eq!(list.len(), 1, "one credential (upsert, not insert)");
    assert_eq!(list[0].password, "s3cr3t", "password decrypted on read");
    assert_eq!(list[0].username, "alice");

    // The stored value is sealed (not plaintext) — verify directly.
    let raw: String = d
        .query_one(
            "SELECT password FROM task_credentials WHERE id=$1",
            &[&created.id],
        )
        .await
        .expect("raw")
        .get(0);
    assert_ne!(raw, "s3cr3t", "stored password must be sealed at rest");
    assert!(
        artex::db::secrets::is_sealed(&artex::db::secrets::COL_CRED_PASS, &raw),
        "stored password carries the sealed prefix"
    );

    // Delete.
    d.delete_task_credential(task_id, created.id)
        .await
        .expect("delete");
    assert!(d.task_credentials(task_id).await.expect("list2").is_empty());

    // Delete of a missing id errors.
    let missing = d.delete_task_credential(task_id, created.id).await;
    assert!(missing.is_err(), "deleting a missing credential errors");

    // Cascade cleanup of the task + exploration.
    d.execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await
        .expect("cleanup task");
    d.execute(
        "DELETE FROM explorations WHERE goal=$1",
        &[&format!("t-cred-goal-{stamp}")],
    )
    .await
    .expect("cleanup exploration");
}

/// `TaskCredentialInput::normalize` validation.
#[test]
fn task_credential_input_normalize() {
    // Missing domain / username → invalid sentinel.
    let e = TaskCredentialInput {
        domain: "  ".into(),
        username: "u".into(),
        ..Default::default()
    }
    .normalize()
    .expect_err("empty domain invalid");
    assert!(e.to_string().contains(ERR_TASK_CREDENTIAL_INVALID), "{e:?}");

    // source=agent with empty password → error.
    let e = TaskCredentialInput {
        domain: "d".into(),
        username: "u".into(),
        password: String::new(),
        source: "agent".into(),
        note: String::new(),
    }
    .normalize()
    .expect_err("agent needs password");
    assert!(
        e.to_string().contains("agent-recorded credentials"),
        "{e:?}"
    );

    // source=user with empty password → ok; trims + defaults source.
    let ok = TaskCredentialInput {
        domain: " d ".into(),
        username: " u ".into(),
        password: String::new(),
        source: String::new(),
        note: " n ".into(),
    }
    .normalize()
    .expect("user empty password ok");
    assert_eq!(ok.domain, "d");
    assert_eq!(ok.username, "u");
    assert_eq!(ok.note, "n");
    assert_eq!(ok.source, "user");
}
