//! `refsmap.rs` — behavioral-parity Rust port of the ref-map helpers and the archive
//! pointer tag from the external `norma/noa` package: `RefForRaw`, `RawForRef`,
//! `HighestUsedIndex` (over `MessageRefMap`) and `ArchiveTag`.
//!
//! `MessageRefMap` is Go's two-way index between host message ids and the `mNNNNN`
//! refs the model addresses them by; only the relevant map is passed to each helper.
//! All functions are pure and deterministic.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::port::{BLOCKED_REF, ref_to_index};

/// `noa.RefForRaw` — resolve a host message id to its assigned ref (or `BLOCKED`).
/// Returns `(ref, ok)`; `ok` is false (with an empty ref) when the id is unknown.
pub fn ref_for_raw(by_raw: &HashMap<String, String>, raw_id: &str) -> (String, bool) {
    match by_raw.get(raw_id) {
        Some(r) => (r.clone(), true),
        None => (String::new(), false),
    }
}

/// `noa.RawForRef` — resolve a ref back to its host message id. Returns `(raw, ok)`.
pub fn raw_for_ref(by_ref: &HashMap<String, String>, r: &str) -> (String, bool) {
    match by_ref.get(r) {
        Some(v) => (v.clone(), true),
        None => (String::new(), false),
    }
}

/// `noa.HighestUsedIndex` — the largest ref index handed out so far, skipping
/// `BLOCKED` entries (which occupy no number).
pub fn highest_used_index(by_raw: &HashMap<String, String>) -> i64 {
    let mut highest = 0;
    for r in by_raw.values() {
        if r == BLOCKED_REF {
            continue;
        }
        let (i, ok) = ref_to_index(r);
        if ok && i > highest {
            highest = i;
        }
    }
    highest
}

/// The subset of `noa.CompressionBlock` that `ArchiveTag` reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArchiveBlock {
    pub block_id: String,
    pub tier: i64,
    pub start_ref: String,
    pub end_ref: String,
    pub archive_path: String,
}

/// `noa.ArchiveTag` — the `<noa-archive …/>` pointer that lets the model (or a
/// human) find a block's original content on disk.
pub fn archive_tag(b: &ArchiveBlock) -> String {
    format!(
        "<noa-archive block=\"{}\" tier=\"{}\" range=\"{}-{}\" path=\"{}\"/>",
        b.block_id, b.tier, b.start_ref, b.end_ref, b.archive_path
    )
}
