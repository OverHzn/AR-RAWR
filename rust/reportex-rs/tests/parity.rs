//! Parity harness for `reportex-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `report/` helpers by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use reportex_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn arg_str(args: &[Value], i: usize) -> String {
    args[i].as_str().expect("string arg").to_string()
}

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args[i].as_i64().expect("int arg")
}

fn arg_bool(args: &[Value], i: usize) -> bool {
    args[i].as_bool().expect("bool arg")
}

/// Decode a recorded `report.Quality` (PascalCase) into the Rust model. Absent
/// fields default to zero/false, matching Go's zero-value struct.
fn decode_quality(v: &Value) -> reportex_rs::port::Quality {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let i = |k: &str| v[k].as_i64().unwrap_or(0);
    let b = |k: &str| v[k].as_bool().unwrap_or(false);
    reportex_rs::port::Quality {
        poe: i("PoE"),
        verification: s("Verification"),
        verification_note: s("VerificationNote"),
        confidence_rationale: s("ConfidenceRationale"),
        counterevidence: s("Counterevidence"),
        traffic_count: i("TrafficCount"),
        record_id: i("RecordID"),
        missing: b("Missing"),
        unavailable: b("Unavailable"),
    }
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "nz" => json!(nz(&arg_str(args, 0), &arg_str(args, 1))),
        "oneLine" => json!(one_line(&arg_str(args, 0))),
        "unbalancedFence" => json!(unbalanced_fence(&arg_str(args, 0))),
        "staleReportNotice" => json!(stale_report_notice(arg_bool(args, 0))),
        "reportStale" => json!(report_stale(
            &arg_str(args, 0),
            arg_i64(args, 1),
            arg_i64(args, 2),
        )),
        "verificationLabel" => json!(verification_label(&arg_str(args, 0))),
        "detailedReportLimit" => json!(detailed_report_limit(
            &arg_str(args, 0),
            &arg_str(args, 1),
            arg_i64(args, 2),
        )),
        "qualityBullets" => {
            let q = decode_quality(&args[0]);
            json!(quality_bullets(&q, arg_bool(args, 1)))
        }
        "qualityNarrative" => {
            let q = decode_quality(&args[0]);
            json!(quality_narrative(&q))
        }
        other => panic!("no dispatcher arm for {other}"),
    }
}

#[test]
fn parity_all_cases() {
    let all = cases();
    assert!(!all.is_empty(), "cases.jsonl must not be empty");
    let mut mismatches: Vec<String> = Vec::new();
    for c in &all {
        let name = c["fn"].as_str().expect("fn");
        let args: Vec<Value> = c["args"].as_array().expect("args array").clone();
        if c.get("error").is_some() {
            mismatches.push(format!("UNEXPECTED error case {name} args={args:?}"));
            continue;
        }
        let actual = dispatch(name, &args);
        let expected = &c["ok"];
        if &actual != expected {
            mismatches.push(format!(
                "{name} args={args:?}\n   expected={expected}\n   actual  ={actual}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} / {} cases mismatched:\n\n{}",
        mismatches.len(),
        all.len(),
        mismatches.join("\n")
    );
}
