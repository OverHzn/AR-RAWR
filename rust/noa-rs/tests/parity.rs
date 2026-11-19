//! Parity harness for `noa-rs`. Replays `cases.jsonl` (recorded from the real Go
//! `norma/noa` helpers by `record.sh`) against the Rust port.

use serde_json::Value;

use noa_rs::blocks::{CompressionBlock, render_summary_message};
use noa_rs::breakdown::compute_context_breakdown;
use noa_rs::buildranges::build_compressible_ranges;
use noa_rs::nudgetext::{
    BlockSpan, TierTargetBlock, format_block_map, format_breakdown, format_tier_target_blocks,
};
use noa_rs::panel::{ApplyResult, PanelBlock, PanelState, format_panel};
use noa_rs::parseargs::parse_compress_args;
use noa_rs::port::*;
use noa_rs::protection::{is_message_protected, is_never_preserve_recent_tool, match_tool_pattern};
use noa_rs::protrefs::compute_protected_refs;
use noa_rs::ranges::{ProtectedRange, format_ranges};
use noa_rs::recommend::{RangeInfo, merge_ranges_to_threshold, viable_ranges};
use noa_rs::refsmap::{ArchiveBlock, archive_tag, highest_used_index, raw_for_ref, ref_for_raw};
use noa_rs::schema::compress_tool_schema;
use noa_rs::state::{StateBlock, active_block_spans, active_blocks, covered_message_ids};
use noa_rs::tier::{decide_tier, is_terminal_rewrite};

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
        "FormatRanges" => {
            let comp = decode_ranges(&args[0]);
            let prot = decode_protected(&args[1]);
            serde_json::to_value(format_ranges(&comp, &prot)).expect("serialize string")
        }
        "FormatPanel" => {
            let before = args[1].as_i64().expect("before");
            let after = args[2].as_i64().expect("after");
            let r = ApplyResult {
                blocks_created: decode_panel_blocks(&args[3]),
                surviving_block_ids: args[4]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                errors: args[5]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                warnings: args[6]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            let state = decode_panel_state(&args[7]);
            serde_json::to_value(format_panel(before, after, &r, &state)).expect("serialize string")
        }
        "ActiveBlocks" => {
            let blocks = decode_state_blocks(&args[0]);
            let out: Vec<Value> = active_blocks(&blocks)
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "BlockID": b.block_id,
                        "Tier": b.tier,
                        "StartRef": b.start_ref,
                        "EndRef": b.end_ref,
                        "Active": b.active,
                        "EffectiveMessageIDs": b.effective_message_ids,
                    })
                })
                .collect();
            Value::Array(out)
        }
        "CoveredMessageIDs" => {
            let blocks = decode_state_blocks(&args[0]);
            let set = covered_message_ids(&blocks);
            serde_json::to_value(set).expect("serialize set")
        }
        "ActiveBlockSpans" => {
            let blocks = decode_state_blocks(&args[0]);
            let spans = active_block_spans(&blocks);
            let out: Vec<Value> = spans
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "BlockID": s.block_id,
                        "Tier": s.tier,
                        "StartRef": s.start_ref,
                        "EndRef": s.end_ref,
                    })
                })
                .collect();
            Value::Array(out)
        }
        "DecideTier" => {
            let kind = args[0]["Kind"].as_str().expect("Kind");
            let nested: Vec<String> = args[0]["NestedBlockIDs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let max_tier = args[0]["MaxTier"].as_i64().expect("MaxTier");
            let blocks = decode_state_blk_arg(&args[1]);
            let d = decide_tier(kind, &nested, max_tier, &blocks);
            // Go nil-slice vs []: empty consumed/surviving render as null.
            let null_or_arr = |v: &Vec<String>| {
                if v.is_empty() {
                    Value::Null
                } else {
                    serde_json::to_value(v).expect("serialize ids")
                }
            };
            serde_json::json!({
                "TargetTier": d.target_tier,
                "OutputTier": d.output_tier,
                "ConsumedBlockIDs": null_or_arr(&d.consumed_block_ids),
                "SurvivingBlockIDs": null_or_arr(&d.surviving_block_ids),
            })
        }
        "IsTerminalRewrite" => {
            let target_tier = args[0]["TargetTier"].as_i64().expect("TargetTier");
            let direct_count = args[0]["DirectCount"].as_i64().expect("DirectCount");
            let max_tier = args[0]["MaxTier"].as_i64().expect("MaxTier");
            serde_json::to_value(is_terminal_rewrite(target_tier, direct_count, max_tier))
                .expect("serialize bool")
        }
        "ComputeProtectedRefs" => {
            let msgs = decode_core_messages(&args[0]);
            let blocks = decode_state_blocks(&args[2]);
            let by_raw = args[1]["ByRaw"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .unwrap_or_default();
            let p_msg = args[3]["PreserveRecentMessages"].as_i64().expect("pMsg");
            let p_tok = args[3]["PreserveRecentTokens"].as_i64().expect("pTok");
            let set = compute_protected_refs(&msgs, &blocks, &by_raw, p_msg, p_tok);
            serde_json::to_value(set).expect("serialize set")
        }
        "BuildCompressibleRanges" => {
            let msgs = decode_core_messages(&args[0]);
            let by_raw = args[1]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .unwrap_or_default();
            let blocks = args[2]
                .as_array()
                .map(|arr| decode_block_array(arr))
                .unwrap_or_default();
            let protected_tools: Vec<String> = args[3]["ProtectedTools"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let protected_refs: std::collections::BTreeSet<String> = args[4]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let (ranges, protected) = build_compressible_ranges(
                &msgs,
                &blocks,
                &by_raw,
                &protected_tools,
                &protected_refs,
                noa_rs::port::default_count_tokens,
            );
            // Go nil-slice: empty ranges/protected render as null.
            let ranges_v = if ranges.is_empty() {
                Value::Null
            } else {
                serde_json::to_value(&ranges).expect("serialize ranges")
            };
            let protected_v = if protected.is_empty() {
                Value::Null
            } else {
                serde_json::to_value(&protected).expect("serialize protected")
            };
            serde_json::json!([ranges_v, protected_v])
        }
        "ComputeContextBreakdown" => {
            let msgs = decode_core_messages(&args[0]);
            let map = compute_context_breakdown(&msgs, noa_rs::port::default_count_tokens);
            serde_json::to_value(map).expect("serialize breakdown")
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

/// Decode a recorded `[]ProtectedRange` (nil -> empty) into `Vec<ProtectedRange>`.
fn decode_protected(v: &Value) -> Vec<ProtectedRange> {
    if v.is_null() {
        return Vec::new();
    }
    v.as_array()
        .expect("protected array")
        .iter()
        .map(|p| ProtectedRange {
            start_ref: p["StartRef"].as_str().unwrap_or("").to_string(),
            end_ref: p["EndRef"].as_str().unwrap_or("").to_string(),
            count: p["Count"].as_i64().unwrap_or(0),
            tokens: p["Tokens"].as_i64().unwrap_or(0),
            tools: p["Tools"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// Decode recorded `[]CompressionBlock` (subset) into `Vec<PanelBlock>`; null -> empty.
fn decode_panel_blocks(v: &Value) -> Vec<PanelBlock> {
    if v.is_null() {
        return Vec::new();
    }
    v.as_array()
        .expect("blocks array")
        .iter()
        .map(|b| PanelBlock {
            block_id: b["BlockID"].as_str().unwrap_or("").to_string(),
            tier: b["Tier"].as_i64().unwrap_or(0),
            start_ref: b["StartRef"].as_str().unwrap_or("").to_string(),
            end_ref: b["EndRef"].as_str().unwrap_or("").to_string(),
            archive_path: b["ArchivePath"].as_str().unwrap_or("").to_string(),
            effective_message_ids: b["EffectiveMessageIDs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// Decode a recorded CompressionState (subset) into `PanelState`.
fn decode_panel_state(v: &Value) -> PanelState {
    let blocks = v["Blocks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|b| {
                    (
                        b["BlockID"].as_str().unwrap_or("").to_string(),
                        b["Tier"].as_i64().unwrap_or(0),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let by_ref = v["MessageRefs"]["ByRef"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    PanelState { blocks, by_ref }
}

/// Decode a recorded CompressionState's `Blocks` into `Vec<StateBlock>` (subset).
fn decode_block_array(arr: &[Value]) -> Vec<StateBlock> {
    arr.iter()
        .map(|b| StateBlock {
            block_id: b["BlockID"].as_str().unwrap_or("").to_string(),
            tier: b["Tier"].as_i64().unwrap_or(0),
            start_ref: b["StartRef"].as_str().unwrap_or("").to_string(),
            end_ref: b["EndRef"].as_str().unwrap_or("").to_string(),
            active: b["Active"].as_bool().unwrap_or(false),
            effective_message_ids: b["EffectiveMessageIDs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

fn decode_state_blocks(v: &Value) -> Vec<StateBlock> {
    v["Blocks"]
        .as_array()
        .map(|arr| decode_block_array(arr))
        .unwrap_or_default()
}

/// Decode the recorded `[]stateBlk` arg (an array of {BlockID,Tier,Active}) into
/// `Vec<StateBlock>`; null -> empty. Fields the tier functions do not read stay default.
fn decode_state_blk_arg(v: &Value) -> Vec<StateBlock> {
    if v.is_null() {
        return Vec::new();
    }
    v.as_array()
        .expect("stateBlk array")
        .iter()
        .map(|b| StateBlock {
            block_id: b["BlockID"].as_str().unwrap_or("").to_string(),
            tier: b["Tier"].as_i64().unwrap_or(0),
            start_ref: String::new(),
            end_ref: String::new(),
            active: b["Active"].as_bool().unwrap_or(false),
            effective_message_ids: Vec::new(),
        })
        .collect()
}

/// Decode a recorded `[]CoreMessage` into `Vec<CoreMessage>`.
fn decode_core_messages(v: &Value) -> Vec<noa_rs::port::CoreMessage> {
    if v.is_null() {
        return Vec::new();
    }
    v.as_array()
        .expect("messages array")
        .iter()
        .map(|m| noa_rs::port::CoreMessage {
            id: m["ID"].as_str().unwrap_or("").to_string(),
            role: m["Role"].as_str().unwrap_or("").to_string(),
            content_type: m["ContentType"].as_str().unwrap_or("").to_string(),
            text: m["Text"].as_str().unwrap_or("").to_string(),
            tool_name: m["ToolName"].as_str().unwrap_or("").to_string(),
            tool_call_id: m["ToolCallID"].as_str().unwrap_or("").to_string(),
        })
        .collect()
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
