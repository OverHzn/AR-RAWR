//! Differential tests for cold-digest persistence — fresh spec for the
//! `*ExplorationStore` digest methods in `db/digest.go` (BumpRound / RoundNo /
//! ColdStamps / ApplyStampOps / ContentVersions / ActiveDigests / AddDigest /
//! CoveredMembers / DigestMembers / SupersedeDigests / NodeAssets).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::StampOp;
use artex::db::DB;
use serde_json::json;

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

#[tokio::test]
async fn exploration_cold_digest() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let exp_id = d
        .create_exploration("desc", &format!("t-digest-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- RoundNo / BumpRound.
    let r0 = store.round_no().await.expect("round 0");
    assert_eq!(r0, 0, "fresh exploration round is 0");
    let r1 = store.bump_round().await.expect("bump");
    assert_eq!(r1, 1);
    assert_eq!(store.round_no().await.expect("round"), 1);

    // ---- ColdStamps / ApplyStampOps / ContentVersions on a few nodes.
    let i1 = store
        .add_intent(&json!({"summary":"i1"}), 1, &[], "planner")
        .await
        .expect("i1");
    let i2 = store
        .add_intent(&json!({"summary":"i2"}), 1, &[], "planner")
        .await
        .expect("i2");
    let f1 = store
        .add_node("fact", &json!({"text":"f1"}), 0, "confirmed", "w", &[])
        .await
        .expect("f1");

    // Set cold_since_round on i1 and f1; leave i2 hot.
    store
        .apply_stamp_ops(&[
            StampOp {
                id: i1,
                set: true,
                round: 1,
            },
            StampOp {
                id: f1,
                set: true,
                round: 1,
            },
        ])
        .await
        .expect("stamp");
    let stamps = store.cold_stamps().await.expect("stamps");
    assert_eq!(stamps.get(&i1), Some(&Some(1)), "i1 stamped round 1");
    assert_eq!(stamps.get(&f1), Some(&Some(1)), "f1 stamped round 1");
    assert_eq!(stamps.get(&i2), Some(&None), "i2 is hot (unstamped)");
    // Only intent/fact nodes appear.
    assert!(!stamps.contains_key(&0), "no stray keys");

    // Clear i1's stamp.
    store
        .apply_stamp_ops(&[StampOp {
            id: i1,
            set: false,
            round: 0,
        }])
        .await
        .expect("clear");
    let stamps = store.cold_stamps().await.expect("stamps 2");
    assert_eq!(stamps.get(&i1), Some(&None), "i1 cleared");

    // ApplyStampOps with empty input is a no-op.
    store.apply_stamp_ops(&[]).await.expect("empty stamps");

    // ---- ContentVersions: present for every node.
    let versions = store.content_versions().await.expect("versions");
    assert_eq!(versions.get(&i1), Some(&0));
    assert_eq!(versions.get(&i2), Some(&0));
    assert_eq!(versions.get(&f1), Some(&0));

    // ---- NodeAssets: anchor i1 and f1 to a (synthetic) asset, read back.
    let asset_id = d
        .query_one(
            "INSERT INTO assets(type, domain) VALUES ('subdomain', $1) RETURNING id",
            &[&format!("asset-{stamp}.example")],
        )
        .await
        .expect("asset")
        .get::<_, i64>(0);
    store.anchor(i1, asset_id).await.expect("anchor i1");
    store.anchor(f1, asset_id).await.expect("anchor f1");
    let node_assets = store.node_assets(&[i1, i2, f1]).await.expect("node assets");
    assert_eq!(node_assets.get(&i1), Some(&vec![asset_id]));
    assert_eq!(node_assets.get(&f1), Some(&vec![asset_id]));
    assert!(!node_assets.contains_key(&i2), "i2 has no anchors");
    assert!(store.node_assets(&[]).await.expect("empty").is_empty());

    // ---- AddDigest + ActiveDigests + DigestMembers + CoveredMembers.
    let member_a = store
        .add_intent(&json!({"summary":"a"}), 1, &[], "planner")
        .await
        .expect("a");
    let member_b = store
        .add_intent(&json!({"summary":"b"}), 1, &[], "planner")
        .await
        .expect("b");
    let digest_payload = json!({"body":"cold", "generation":1, "member_ids":[member_a, member_b]});
    let digest_id = store
        .add_digest(&digest_payload, &[member_a, member_b])
        .await
        .expect("add digest");

    let active = store.active_digests().await.expect("active digests");
    assert!(active.iter().any(|n| n.id == digest_id), "digest is active");
    assert!(active
        .iter()
        .all(|n| n.kind == "digest" && n.state == "active"));

    // DigestMembers: sorted.
    let members = store.digest_members(digest_id).await.expect("members");
    let mut want = vec![member_a, member_b];
    want.sort_unstable();
    assert_eq!(members, want, "digest members sorted");

    // CoveredMembers: each member maps to the covering digest (lowest id wins).
    let covered = store.covered_members().await.expect("covered");
    assert_eq!(covered.get(&member_a), Some(&digest_id));
    assert_eq!(covered.get(&member_b), Some(&digest_id));

    // ---- Add a SECOND digest covering member_a; lowest digest id wins.
    let digest2 = store
        .add_digest(&json!({"body":"newer"}), &[member_a])
        .await
        .expect("add digest 2");
    let covered = store.covered_members().await.expect("covered 2");
    assert_eq!(
        covered.get(&member_a),
        Some(&digest_id),
        "lowest digest id wins for a doubly-covered member"
    );

    // ---- SupersedeDigests: digest2 retires, its covers edge removed.
    store
        .supersede_digests(&[digest2])
        .await
        .expect("supersede");
    let active = store.active_digests().await.expect("active after");
    assert!(
        !active.iter().any(|n| n.id == digest2),
        "digest2 superseded"
    );
    assert!(
        active.iter().any(|n| n.id == digest_id),
        "digest1 still active"
    );
    // digest2's members edges are gone; member_a now only covered by digest1.
    let covered = store.covered_members().await.expect("covered after");
    assert_eq!(covered.get(&member_a), Some(&digest_id));

    // SupersedeDigests with empty input is a no-op.
    store.supersede_digests(&[]).await.expect("empty supersede");

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await;
}
