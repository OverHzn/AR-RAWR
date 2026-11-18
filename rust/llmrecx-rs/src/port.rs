//! Rust port of the unexported, pure helpers in the Go `llmrec/` package.

/// `parseSession` (`llmrec/session.go`): split an `exp<N>-<role>[-suffix]` session
/// key into `(taskID, worker)`.
///
/// - a key not starting with `exp` → `("", "")`;
/// - otherwise the part after `exp` up to the first `-` is `taskID` (the whole rest
///   if there is no `-`, with an empty `worker`);
/// - the next segment, up to the following `-` (or the end), is `worker`.
pub fn parse_session(s: &str) -> (String, String) {
    let rest = match s.strip_prefix("exp") {
        Some(r) => r,
        None => return (String::new(), String::new()),
    };
    match rest.find('-') {
        Some(i) => {
            let task_id = &rest[..i];
            let after = &rest[i + 1..];
            let worker = match after.find('-') {
                Some(j) => &after[..j],
                None => after,
            };
            (task_id.to_string(), worker.to_string())
        }
        None => (rest.to_string(), String::new()),
    }
}
