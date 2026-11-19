//! Parity harness for `noa-rs`. Replays `cases.jsonl` (recorded from the real Go
//! `norma/noa` helpers by `record.sh`) against the Rust port.

use serde_json::Value;

use noa_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

/// Encode a Rust `(value, ok)` pair the way the oracle records it: `{"v":..,"ok":..}`.
fn pair_json<T: serde::Serialize>(v: &T, ok: bool) -> Value {
    serde_json::json!({ "v": v, "ok": ok })
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "FormatTokens" => {
            let n = args[0].as_i64().expect("int arg");
            serde_json::to_value(format_tokens(n)).expect("serialize string")
        }
        "IndexToRef" => {
            let i = args[0].as_i64().expect("int arg");
            serde_json::to_value(index_to_ref(i)).expect("serialize string")
        }
        "RefToIndex" => {
            let s = args[0].as_str().expect("string arg");
            let (v, ok) = ref_to_index(s);
            pair_json(&v, ok)
        }
        "ParseBlockID" => {
            let s = args[0].as_str().expect("string arg");
            let (v, ok) = parse_block_id(s);
            pair_json(&v, ok)
        }
        "SummaryMessageID" => {
            let b = args[0].as_str().expect("string arg");
            serde_json::to_value(summary_message_id(b)).expect("serialize string")
        }
        "BlockIDFromSummaryID" => {
            let s = args[0].as_str().expect("string arg");
            let (v, ok) = block_id_from_summary_id(s);
            pair_json(&v, ok)
        }
        "DefaultCountTokens" => {
            let s = args[0].as_str().expect("string arg");
            serde_json::to_value(default_count_tokens(s)).expect("serialize int")
        }
        "EstimateTokensFast" => {
            let s = args[0].as_str().expect("string arg");
            serde_json::to_value(estimate_tokens_fast(s)).expect("serialize int")
        }
        "ArchiveFileName" => {
            let block = args[0].as_str().expect("block arg");
            let start = args[1].as_str().expect("start arg");
            let end = args[2].as_str().expect("end arg");
            serde_json::to_value(archive_file_name(block, start, end)).expect("serialize string")
        }
        "ArchiveTierDir" => {
            let t = args[0].as_i64().expect("int arg");
            serde_json::to_value(archive_tier_dir(t)).expect("serialize string")
        }
        "PanelBlockCount" => {
            let p = args[0].as_str().expect("string arg");
            serde_json::to_value(panel_block_count(p)).expect("serialize int")
        }
        "DefaultConfig" => {
            let lim = args[0].as_i64().expect("int arg");
            serde_json::to_value(default_config(lim)).expect("serialize config")
        }
        "ValidateConfig" => {
            let cfg: Config = serde_json::from_value(args[0].clone()).expect("config arg decode");
            let warnings = validate_config(&cfg);
            // Go declares `var w []string` and appends; with no warnings it stays
            // nil and json.Marshal renders `null`, not `[]`. Mirror that.
            if warnings.is_empty() {
                Value::Null
            } else {
                serde_json::to_value(warnings).expect("serialize warnings")
            }
        }
        "ComputeTurnGroups" => {
            let msgs: Vec<CoreMessage> =
                serde_json::from_value(args[0].clone()).expect("messages arg decode");
            serde_json::to_value(compute_turn_groups(&msgs)).expect("serialize groups")
        }
        "WithdrawSplitTurns" => {
            let msgs: Vec<CoreMessage> =
                serde_json::from_value(args[0].clone()).expect("messages arg decode");
            let folded: Option<std::collections::HashMap<String, bool>> =
                serde_json::from_value(args[1].clone()).expect("folded arg decode");
            let (out, withdrawn) = withdraw_split_turns(&msgs, folded);
            // Recorded as a two-element array [out, withdrawn]; each is null when
            // its Go counterpart is nil.
            serde_json::json!([
                out.map(|m| serde_json::to_value(m).unwrap())
                    .unwrap_or(Value::Null),
                withdrawn
                    .map(|w| serde_json::to_value(w).unwrap())
                    .unwrap_or(Value::Null),
            ])
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
