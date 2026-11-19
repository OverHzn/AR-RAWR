//! Parity harness for `noa-rs`. Replays `cases.jsonl` (recorded from the real Go
//! `norma/noa` helpers by `record.sh`) against the Rust port.

use serde_json::Value;

use noa_rs::blocks::{CompressionBlock, render_summary_message};
use noa_rs::nudgetext::{
    BlockSpan, TierTargetBlock, format_block_map, format_breakdown, format_tier_target_blocks,
};
use noa_rs::parseargs::parse_compress_args;
use noa_rs::port::*;
use noa_rs::protection::{is_message_protected, is_never_preserve_recent_tool, match_tool_pattern};
use noa_rs::recommend::{RangeInfo, merge_ranges_to_threshold, viable_ranges};
use noa_rs::refsmap::{ArchiveBlock, archive_tag, highest_used_index, raw_for_ref, ref_for_raw};
use noa_rs::schema::compress_tool_schema;

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
        "AdjustBoundariesForToolPairs" => {
            let start = args[0].as_i64().expect("start");
            let end = args[1].as_i64().expect("end");
            let msgs: Vec<CoreMessage> =
                serde_json::from_value(args[2].clone()).expect("messages arg decode");
            let max_scan = args[3].as_i64().expect("max_scan");
            let (s, e) = adjust_boundaries_for_tool_pairs(start, end, &msgs, max_scan);
            serde_json::json!([s, e])
        }
        "AdjustBoundariesForReasoningPairs" => {
            let start = args[0].as_i64().expect("start");
            let end = args[1].as_i64().expect("end");
            let msgs: Vec<CoreMessage> =
                serde_json::from_value(args[2].clone()).expect("messages arg decode");
            let (s, e) = adjust_boundaries_for_reasoning_pairs(start, end, &msgs);
            serde_json::json!([s, e])
        }
        "ApplyPairBoundaryAdjustments" => {
            let start = args[0].as_i64().expect("start");
            let end = args[1].as_i64().expect("end");
            let msgs: Vec<CoreMessage> =
                serde_json::from_value(args[2].clone()).expect("messages arg decode");
            let (s, e) = apply_pair_boundary_adjustments(start, end, &msgs);
            serde_json::json!([s, e])
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
        "ParseCompressArgs" => {
            let raw = args[0].as_str().expect("raw string arg");
            serde_json::to_value(parse_compress_args(raw.as_bytes())).expect("serialize result")
        }
        "FormatBreakdown" => {
            let obj = args[0].as_object().expect("breakdown arg object");
            let growth = obj["Growth"].as_i64().expect("growth");
            let cb = obj["ContextBreakdown"]
                .as_object()
                .expect("context breakdown");
            let breakdown: Vec<(String, i64)> = cb
                .iter()
                .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
                .collect();
            serde_json::to_value(format_breakdown(&breakdown, growth)).expect("serialize string")
        }
        "FormatBlockMap" => {
            let spans: Vec<BlockSpan> = serde_json::from_value::<Vec<Value>>(args[0].clone())
                .expect("block spans decode")
                .into_iter()
                .map(|v| BlockSpan {
                    block_id: v["BlockID"].as_str().unwrap_or("").to_string(),
                    tier: v["Tier"].as_i64().unwrap_or(0),
                    start_ref: v["StartRef"].as_str().unwrap_or("").to_string(),
                    end_ref: v["EndRef"].as_str().unwrap_or("").to_string(),
                })
                .collect();
            serde_json::to_value(format_block_map(&spans)).expect("serialize string")
        }
        "FormatTierTargetBlocks" => {
            let blocks: Vec<TierTargetBlock> =
                serde_json::from_value::<Vec<Value>>(args[0].clone())
                    .expect("blocks decode")
                    .into_iter()
                    .map(|v| TierTargetBlock {
                        block_id: v["BlockID"].as_str().unwrap_or("").to_string(),
                        tier: v["Tier"].as_i64().unwrap_or(0),
                        topic: v["Topic"].as_str().unwrap_or("").to_string(),
                        summary: v["Summary"].as_str().unwrap_or("").to_string(),
                        effective_message_ids: v["EffectiveMessageIDs"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        compressed_tokens: v["CompressedTokens"].as_i64().unwrap_or(0),
                    })
                    .collect();
            serde_json::to_value(format_tier_target_blocks(&blocks)).expect("serialize string")
        }
        "MergeRangesToThreshold" => {
            let ranges: Vec<RangeInfo> = decode_ranges(&args[0]);
            let min_chars = args[1].as_i64().expect("min_chars");
            serde_json::to_value(merge_ranges_to_threshold(&ranges, min_chars))
                .expect("serialize ranges")
        }
        "ViableRanges" => {
            let ranges: Vec<RangeInfo> = decode_ranges(&args[0]);
            serde_json::to_value(viable_ranges(&ranges)).expect("serialize ranges")
        }
        "CompressToolSchema" => compress_tool_schema(),
        "ArchiveTag" => {
            let a = &args[0];
            let b = ArchiveBlock {
                block_id: a["ID"].as_str().unwrap_or("").to_string(),
                tier: a["Tier"].as_i64().unwrap_or(0),
                start_ref: a["Start"].as_str().unwrap_or("").to_string(),
                end_ref: a["End"].as_str().unwrap_or("").to_string(),
                archive_path: a["Path"].as_str().unwrap_or("").to_string(),
            };
            serde_json::to_value(archive_tag(&b)).expect("serialize string")
        }
        "HighestUsedIndex" => {
            let m: std::collections::HashMap<String, String> =
                serde_json::from_value(args[0].clone()).expect("byRaw decode");
            serde_json::to_value(highest_used_index(&m)).expect("serialize int")
        }
        "RefForRaw" => {
            let m: std::collections::HashMap<String, String> =
                serde_json::from_value(args[0].clone()).expect("byRaw decode");
            let id = args[1].as_str().expect("raw id");
            let (r, ok) = ref_for_raw(&m, id);
            serde_json::json!({ "ref": r, "ok": ok })
        }
        "RawForRef" => {
            let m: std::collections::HashMap<String, String> =
                serde_json::from_value(args[0].clone()).expect("byRef decode");
            let r = args[1].as_str().expect("ref");
            let (raw, ok) = raw_for_ref(&m, r);
            serde_json::json!({ "raw": raw, "ok": ok })
        }
        "MatchToolPattern" => {
            let p = args[0].as_str().expect("pattern");
            let tn = args[1].as_str().expect("tool name");
            serde_json::to_value(match_tool_pattern(p, tn)).expect("serialize bool")
        }
        "IsNeverPreserveRecentTool" => {
            let tn = args[0].as_str().expect("tool name");
            serde_json::to_value(is_never_preserve_recent_tool(tn)).expect("serialize bool")
        }
        "IsMessageProtected" => {
            let a = &args[0];
            let tn = a["ToolName"].as_str().unwrap_or("").to_string();
            let protected: Vec<String> = a["ProtectedTools"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let host: Option<Vec<String>> = a["HostProtected"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            });
            serde_json::to_value(is_message_protected(&tn, &protected, host.as_deref()))
                .expect("serialize bool")
        }
        "RenderSummaryMessage" => {
            let a = &args[0];
            let b = CompressionBlock {
                block_id: a["ID"].as_str().unwrap_or("").to_string(),
                tier: a["Tier"].as_i64().unwrap_or(0),
                topic: a["Topic"].as_str().unwrap_or("").to_string(),
                summary: a["Summary"].as_str().unwrap_or("").to_string(),
                start_ref: a["Start"].as_str().unwrap_or("").to_string(),
                end_ref: a["End"].as_str().unwrap_or("").to_string(),
                archive_path: a["Path"].as_str().unwrap_or("").to_string(),
            };
            serde_json::to_value(render_summary_message(&b)).expect("serialize message")
        }
        other => panic!("no dispatcher arm for {other}"),
    }
}

/// Decode a recorded `[]RangeInfo` (array of objects) into `Vec<RangeInfo>`.
/// A nil slice marshals to `null` in the recorded args; model it as empty.
fn decode_ranges(v: &Value) -> Vec<RangeInfo> {
    if v.is_null() {
        return Vec::new();
    }
    serde_json::from_value(v.clone()).expect("ranges decode")
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
