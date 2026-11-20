//! Finding verification state — port of `db/finding_verification.go`.

use crate::db::error::{Error, Result};
use crate::db::finding_evidence::ERR_FINDING_NOT_FOUND;
use crate::db::DB;

pub const VERIFICATION_VERIFIED: &str = "verified";
pub const VERIFICATION_UNVERIFIED: &str = "unverified";
pub const VERIFICATION_BLOCKED: &str = "blocked";
pub const VERIFICATION_NOT_ASSESSED: &str = "not_assessed";
pub const VERIFICATION_FALSE_POSITIVE: &str = "false_positive";

pub fn valid_verification_status(s: &str) -> bool {
    matches!(
        s,
        VERIFICATION_VERIFIED
            | VERIFICATION_UNVERIFIED
            | VERIFICATION_BLOCKED
            | VERIFICATION_NOT_ASSESSED
            | VERIFICATION_FALSE_POSITIVE
    )
}

impl DB {
    /// `SetFindingVerification`: record verification state, POE level and note.
    /// Does not touch status/report/evidence. Unknown finding -> `ERR_FINDING_NOT_FOUND`.
    pub async fn set_finding_verification(
        &self,
        finding_id: i64,
        status: &str,
        poe: i32,
        note: &str,
    ) -> Result<()> {
        if !valid_verification_status(status) {
            return Err(Error::msg(format!(
                "invalid verification status {status:?}"
            )));
        }
        if !(0..=4).contains(&poe) {
            return Err(Error::msg(format!("invalid POE level {poe} (want 0-4)")));
        }
        let poe_small = poe as i16;
        let n = self
            .execute(
                "UPDATE findings SET verification_status=$2, poe_level=$3, verification_note=$4 WHERE id=$1",
                &[&finding_id, &status, &poe_small, &note],
            )
            .await?;
        if n == 0 {
            return Err(Error::sentinel(
                ERR_FINDING_NOT_FOUND,
                ERR_FINDING_NOT_FOUND,
            ));
        }
        Ok(())
    }
}
