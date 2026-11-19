//! Rust port of the `Capture` / rune-boundary specs in the external `norma/tool`
//! package: `capture_test.go::TestCaptureSpillPointerIsAbsolute`,
//! `capture_test.go::TestCaptureCutsOnRuneBoundaries`, and
//! `utf8_boundary_test.go::TestRuneSafeHelpers`.
//!
//! They pin the output-bounder's contract: oversized output with an `OutputDir`
//! spills the FULL output to an absolute, readable path and returns a tight head +
//! pointer; both the spill head and the head+tail truncation fallback cut on UTF-8
//! rune boundaries (never splitting a multi-byte character); and `runeSafeLen` /
//! `runeSafeStart` align byte offsets to rune boundaries.

use std::path::Path;

use artex::tool::{capture, cut_head, cut_tail, rune_safe_len, rune_safe_start, ToolContext};

/// `extractPointer` (Go test helper): pull the absolute spill path out of a
/// `<persisted-output>` notice, between the `Full output saved to  ` marker and the
/// closing ` </persisted-output>`. Panics (test-fails) if the notice is missing.
fn extract_pointer(out: &str) -> String {
    const MARKER: &str = "Full output saved to  ";
    let i = out
        .find(MARKER)
        .unwrap_or_else(|| panic!("spill notice missing: {out}"));
    let rest = &out[i + MARKER.len()..];
    let j = rest
        .find(" </persisted-output>")
        .unwrap_or_else(|| panic!("spill notice malformed: {out}"));
    rest[..j].trim().to_string()
}

/// `TestCaptureSpillPointerIsAbsolute`: a RELATIVE `OutputDir` under a working
/// directory must come back as an ABSOLUTE pointer, and the file must exist exactly
/// there holding the full output. Go uses `t.Chdir(work)` (process cwd); the Rust
/// port sets `output_dir` to a relative path and resolves it against a controlled
/// cwd via a guarded `set_current_dir` (single test, no parallelism in this binary
/// for that case — it holds a process-wide lock).
#[tokio::test]
async fn capture_spill_pointer_is_absolute() {
    // Serialise the cwd-sensitive case; Rust runs a file's tests on parallel
    // threads, so a process-global cwd change must not race another test.
    static CWD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _g = CWD_LOCK.lock().await;

    let work = std::env::temp_dir().join(format!("artex-cap-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(&work).unwrap();

    let tc = ToolContext {
        working_dir: work.to_string_lossy().to_string(),
        output_dir: "cmd-output".to_string(),
        max_output_chars: 200,
        ..Default::default()
    };
    let out = capture(&tc, &"z".repeat(5000));
    std::env::set_current_dir(&original).unwrap();

    assert!(
        out.contains("<persisted-output>"),
        "oversized output should spill: {out}"
    );
    let reference = extract_pointer(&out);
    assert!(
        Path::new(&reference).is_absolute(),
        "spill pointer must be absolute, got {reference}"
    );
    // Read the spill file before any cleanup: it lives under `work`.
    let body = std::fs::read_to_string(&reference)
        .unwrap_or_else(|e| panic!("spill file not readable at the pointer {reference}: {e}"));
    assert_eq!(
        body.len(),
        5000,
        "spill file holds {} bytes, want 5000",
        body.len()
    );

    let _ = std::fs::remove_dir_all(&work);
}

/// `TestCaptureCutsOnRuneBoundaries`: a bounded result must never carry a
/// half-encoded character — both the spilled head and the head+tail fallback cut
/// on rune boundaries, even when the cut lands mid-character.
#[test]
fn capture_cuts_on_rune_boundaries() {
    // 3-byte runes ("测") with a byte length that forces the cut into the middle of one.
    let long = "测".repeat(400);
    assert_eq!(long.len() % 3, 0, "fixture not aligned");

    let temp = std::env::temp_dir().join(format!("artex-cap-rune-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp);

    for tc in [
        ToolContext {
            max_output_chars: 101,
            ..Default::default()
        }, // truncate fallback, odd byte limit
        ToolContext {
            output_dir: temp.to_string_lossy().to_string(),
            max_output_chars: 101,
            ..Default::default()
        }, // spill head
    ] {
        let out = capture(&tc, &long);
        assert!(
            is_valid_utf8(&out),
            "captured output is not valid UTF-8 (spill={})",
            !tc.output_dir.is_empty()
        );
        assert!(
            out.starts_with('测'),
            "head must start on a rune boundary: {:?}",
            out.chars().take(10).collect::<String>()
        );
    }

    // The fallback keeps a tail too, and that tail must also start on a boundary.
    let out = capture(
        &ToolContext {
            max_output_chars: 101,
            ..Default::default()
        },
        &format!("{long}尾部"),
    );
    assert!(is_valid_utf8(&out));
    assert!(out.ends_with("尾部"), "truncated tail broken: out={out}");
    assert!(
        out.contains("characters truncated]"),
        "truncation marker lost: {out}"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

/// `TestRuneSafeHelpers`: `runeSafeLen` returns the largest prefix length at most
/// `max` that ends on a rune boundary; `runeSafeStart` returns the leading
/// continuation-byte count to drop.
#[test]
fn rune_safe_helpers() {
    // "日" is 3 bytes: E6 97 A5.
    let s = "a日b";
    let b = s.as_bytes();
    for (max, want) in [
        (0usize, ""),
        (1, "a"),
        (2, "a"),
        (3, "a"),
        (4, "a日"),
        (5, "a日b"),
        (9, "a日b"),
    ] {
        let got = &s[..rune_safe_len(b, max)];
        assert_eq!(
            got, want,
            "runeSafeLen({s:?}, {max}) cut to {got:?}, want {want:?}"
        );
    }
    assert_eq!(rune_safe_start(b), 0, "runeSafeStart on an aligned string");

    // Slice into the middle of 日: two continuation bytes must be skipped.
    let mid = &b[2..];
    assert_eq!(rune_safe_start(mid), 2, "runeSafeStart(mid)");
    let aligned = std::str::from_utf8(&mid[rune_safe_start(mid)..]).unwrap();
    assert_eq!(aligned, "b", "aligned tail");

    // cutHead / cutTail agree with the rune-boundary primitives.
    assert_eq!(cut_head(s, 4), "a日");
    assert_eq!(cut_tail(s, 1), "b");
}

/// A local UTF-8 validator (`std::str::from_utf8` succeeds on any `&str` by
/// construction, so this checks the invariant a test reader would assert on the
/// borrowed bytes).
fn is_valid_utf8(s: &str) -> bool {
    std::str::from_utf8(s.as_bytes()).is_ok()
}
