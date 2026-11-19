//! Exploration graph constants — port of `db/constants.go`.
//!
//! Pure string constants for exploration node kinds, digest/intent states, and
//! edge relations. The exploration resource group consumes these; they live in
//! their own module so dependents share one source of truth.

// Exploration node kinds (exploration_nodes.kind).
/// DEPRECATED: legacy task root; new tasks seed an origin fact ([`KIND_FACT`] +
/// [`STATE_ORIGIN`]) instead.
pub const KIND_BEGIN: &str = "begin";
/// A task objective.
pub const KIND_GOAL: &str = "goal";
/// An exploration direction (planner-generated).
pub const KIND_INTENT: &str = "intent";
/// A worker's exploration result/conclusion (incl. negative results), tied to its
/// intent.
pub const KIND_FACT: &str = "fact";
/// A confirmed vulnerability (report_finding), distinct from a fact.
pub const KIND_FINDING: &str = "finding";
pub const KIND_HINT: &str = "hint";
/// A compressed fold of cold intents/facts (cold-digest-spec §1); lossless —
/// members kept, restorable by id.
pub const KIND_DIGEST: &str = "digest";

// Digest node states (kind='digest'). A digest is 'active' while it renders in
// graph_overview; major compaction retires a merged-away segment to 'superseded'
// (its covers edges repointed to the new digest) — cold-digest-spec §5.1.
pub const STATE_DIGEST_ACTIVE: &str = "active";
pub const STATE_DIGEST_SUPERSEDED: &str = "superseded";

/// `StateOrigin` marks the task root fact ([`KIND_FACT`]) seeded at task creation
/// — the exploration graph's origin. Every intent traces back to it. Worker-
/// produced facts use state 'confirmed', so this never collides.
pub const STATE_ORIGIN: &str = "origin";

/// `StateIntentDeleted` marks an intent soft-deleted by the user: it drops out of
/// the frontier and graph_overview like other terminal states, but keeps its node
/// and full lineage. The delete reason lives in
/// `exploration_nodes.delete_reason`.
pub const STATE_INTENT_DELETED: &str = "deleted";

// Exploration edge relations (exploration_edges.rel).
pub const REL_SPAWNS: &str = "spawns";
pub const REL_DERIVED_FROM: &str = "derived_from";
pub const REL_YIELDS: &str = "yields";
pub const REL_PROVES: &str = "proves";
/// `covers` — digest --covers--> member (cold-digest-spec §1); source of truth for
/// "which digest folds node X".
pub const REL_COVERS: &str = "covers";
