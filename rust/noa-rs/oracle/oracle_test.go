// Package noaoracle records ground-truth outputs of the pure, value-typed helpers of
// the external github.com/Autumn-27/norma/noa package for the Rust parity harness.
//
// norma/noa lives in the read-only Go module cache, so record.sh cannot copy an oracle
// into it. Instead this standalone module imports it and prints one JSON object per
// case: {fn, args, ok}. record.sh sorts the lines so the output is byte-reproducible.
// Only exported functions are reachable here.
//
// Batch 9a: FormatTokens, IndexToRef, RefToIndex, ParseBlockID, SummaryMessageID,
//           BlockIDFromSummaryID, DefaultCountTokens, EstimateTokensFast.
// Batch 9b: ArchiveFileName, ArchiveTierDir, PanelBlockCount, DefaultConfig,
//           ValidateConfig (recorded through a shadow struct because Config carries a
//           func field that json.Marshal cannot serialise).
package noaoracle

import (
	"encoding/json"
	"fmt"
	"sort"
	"testing"

	"github.com/Autumn-27/norma/noa"
)

func rec(t *testing.T, fn string, args []any, out any) {
	t.Helper()
	c := map[string]any{"fn": fn, "args": args, "ok": out}
	b, _ := json.Marshal(c)
	fmt.Println(string(b))
}

// two results (value, ok) are recorded as {"v": value, "ok": bool}.
func rec2(t *testing.T, fn string, arg any, v any, ok bool) {
	t.Helper()
	rec(t, fn, []any{arg}, map[string]any{"v": v, "ok": ok})
}

func TestZZOracle9a(t *testing.T) {
	// ---- FormatTokens: exact<1000, one-decimal<10000, whole-thousand>=10000 ----
	for _, n := range []int{
		0, 1, 999,
		1000, 1001, 1500, 1999, 2000, 9499, 9500, 9999,
		10000, 10001, 12345, 12500, 99999, 100000, 999999,
	} {
		rec(t, "FormatTokens", []any{n}, noa.FormatTokens(n))
	}

	// ---- IndexToRef: valid range + out-of-range (empty) ----
	for _, i := range []int{0, 1, 2, 5, 12, 99999, 100000, -1} {
		rec(t, "IndexToRef", []any{i}, noa.IndexToRef(i))
	}

	// ---- RefToIndex: padded, unpadded, upper, spaces, invalid ----
	for _, s := range []string{
		"m00005", "m5", "m00001", "m99999", "M00012", "  m7  ", "m0",
		"m00000", "m100000", "m123456", "m", "mabc", "b5", "b005", "5",
		"m-1", "", "n5", "mm5", "m1x",
	} {
		v, ok := noa.RefToIndex(s)
		rec2(t, "RefToIndex", s, v, ok)
	}

	// ---- ParseBlockID: b-form, bare, tier suffix, invalid ----
	for _, s := range []string{
		"b5", "b005", "5", "b1", "b999999999", "b5(T2)", "b7(t3)",
		"B7", "b0", "0", "x", "", "b", "b-1", "b12345678901", "t2",
		"b5 (T2)", "b5(t2)extra",
	} {
		v, ok := noa.ParseBlockID(s)
		rec2(t, "ParseBlockID", s, v, ok)
	}

	// ---- SummaryMessageID / BlockIDFromSummaryID ----
	for _, b := range []string{"b1", "b5", "b42", "weird", ""} {
		rec(t, "SummaryMessageID", []any{b}, noa.SummaryMessageID(b))
	}
	for _, s := range []string{
		"noa_summary_b5", "noa_summary_b1", "noa_summary_", "noa_summary_weird",
		"b5", "noa_summary_", "prefix_noa_summary_b5", "NOA_SUMMARY_B5",
	} {
		v, ok := noa.BlockIDFromSummaryID(s)
		rec2(t, "BlockIDFromSummaryID", s, v, ok)
	}

	// ---- DefaultCountTokens / EstimateTokensFast ----
	texts := []string{
		"", "a", "abcd", "abcde", "abcdefgh", "hello world",
		"你好世界",           // 4 CJK
		"a你b好c世d界", // mixed
		"こんにちは",           // Hiragana
		"한국어",                 // Hangul
		"emoji😀x",         // astral (1 rune) + ascii
		"Ω≈ç√",             // non-CJK multibyte
	}
	for _, s := range texts {
		rec(t, "DefaultCountTokens", []any{s}, noa.DefaultCountTokens(s))
		rec(t, "EstimateTokensFast", []any{s}, noa.EstimateTokensFast(s))
	}
}

// configJSON mirrors noa.Config's value-typed fields (drops IsToolProtected func).
type tierCfg struct {
	Enabled      bool
	MaxTier      int
	Tier2Trigger int
	Tier3Trigger int
}
type nudgeCfg struct {
	MaxContextLimitPct       float64
	MinContextLimitPct       float64
	EmergencyThresholdPct    float64
	GrowthRatio              float64
	GrowthFloor              int
	GrowthCap                int
	MinGrowthFloor           int
	MinGrowthRatio           float64
	Tier2GrowthMultiplier    float64
	MinPressureBenefitTokens *int
}
type truncateCfg struct{ Threshold float64 }
type compressCfg struct {
	MinCompressRange int
	MinSummaryLength int
	MaxSummaryLength int
}
type configJSON struct {
	ModelContextLimit      int
	Tiers                  tierCfg
	Nudge                  nudgeCfg
	Truncate               truncateCfg
	Compress               compressCfg
	ProtectedTools         []string
	PreserveRecentMessages int
	PreserveRecentTokens   int
	MaxCompressAttempts    int
}

func toConfigJSON(c noa.Config) configJSON {
	return configJSON{
		ModelContextLimit: c.ModelContextLimit,
		Tiers:             tierCfg{c.Tiers.Enabled, int(c.Tiers.MaxTier), c.Tiers.Tier2Trigger, c.Tiers.Tier3Trigger},
		Nudge: nudgeCfg{
			c.Nudge.MaxContextLimitPct, c.Nudge.MinContextLimitPct, c.Nudge.EmergencyThresholdPct,
			c.Nudge.GrowthRatio, c.Nudge.GrowthFloor, c.Nudge.GrowthCap,
			c.Nudge.MinGrowthFloor, c.Nudge.MinGrowthRatio, c.Nudge.Tier2GrowthMultiplier,
			c.Nudge.MinPressureBenefitTokens,
		},
		Truncate:               truncateCfg{c.Truncate.Threshold},
		Compress:               compressCfg{c.Compress.MinCompressRange, c.Compress.MinSummaryLength, c.Compress.MaxSummaryLength},
		ProtectedTools:         c.ProtectedTools,
		PreserveRecentMessages: c.PreserveRecentMessages,
		PreserveRecentTokens:   c.PreserveRecentTokens,
		MaxCompressAttempts:    c.MaxCompressAttempts,
	}
}

func TestZZOracle9b(t *testing.T) {
	// ---- ArchiveFileName / ArchiveTierDir ----
	for _, fc := range []struct{ block, start, end string }{
		{"b5", "m00001", "m00010"},
		{"b42", "m00100", "m00250"},
		{"b1", "m00001", "m00001"},
	} {
		rec(t, "ArchiveFileName", []any{fc.block, fc.start, fc.end}, noa.ArchiveFileName(fc.block, fc.start, fc.end))
	}
	for _, tier := range []int{0, 1, 2, 3} {
		rec(t, "ArchiveTierDir", []any{tier}, noa.ArchiveTierDir(noa.Tier(tier)))
	}

	// ---- PanelBlockCount: count "\bb\d+\(T\d+\)=" occurrences ----
	for _, p := range []string{
		"",
		"no blocks here",
		"  b5(T1)=m00001–m00010 → a.md\n  b6(T2)=m00011–m00020 → b.md",
		"b1(T1)=x b2(T2)=y b3(T3)=z",
		"b12(T2)=something",
		"note: b5 was inside",
		"b5(T1)=",
		"xb5(T1)=y",
		"ab5(T1)=cd",
		"  b99(T12)=z  b100(T3)=w",
		"b5 (T1)=y",
		"b5(T1) =y",
		"B5(T1)=y",
	} {
		rec(t, "PanelBlockCount", []any{p}, noa.PanelBlockCount(p))
	}

	// ---- DefaultConfig: record the shadow JSON for several context limits ----
	for _, lim := range []int{0, 1000, 128000, 200000, 1000000} {
		rec(t, "DefaultConfig", []any{lim}, toConfigJSON(noa.DefaultConfig(lim)))
	}

	// ---- ValidateConfig: default (clean) + hand-built configs tripping each warning ----
	def := noa.DefaultConfig(200000)
	rec(t, "ValidateConfig", []any{toConfigJSON(def)}, noa.ValidateConfig(def))

	bad1 := def
	bad1.ModelContextLimit = 0
	rec(t, "ValidateConfig", []any{toConfigJSON(bad1)}, noa.ValidateConfig(bad1))

	bad2 := def
	bad2.Nudge.MinContextLimitPct = 0.9
	bad2.Nudge.MaxContextLimitPct = 0.5
	rec(t, "ValidateConfig", []any{toConfigJSON(bad2)}, noa.ValidateConfig(bad2))

	bad3 := def
	bad3.Nudge.MaxContextLimitPct = 0.99
	bad3.Nudge.EmergencyThresholdPct = 0.95
	rec(t, "ValidateConfig", []any{toConfigJSON(bad3)}, noa.ValidateConfig(bad3))

	bad4 := def
	bad4.Truncate.Threshold = 0
	rec(t, "ValidateConfig", []any{toConfigJSON(bad4)}, noa.ValidateConfig(bad4))

	bad5 := def
	bad5.Truncate.Threshold = 1.5
	rec(t, "ValidateConfig", []any{toConfigJSON(bad5)}, noa.ValidateConfig(bad5))

	bad6 := def
	bad6.Tiers.Tier2Trigger = 0
	bad6.Tiers.Tier3Trigger = 5
	rec(t, "ValidateConfig", []any{toConfigJSON(bad6)}, noa.ValidateConfig(bad6))

	bad7 := def
	bad7.Tiers.Tier3Trigger = 5
	bad7.Tiers.Tier2Trigger = 10
	rec(t, "ValidateConfig", []any{toConfigJSON(bad7)}, noa.ValidateConfig(bad7))

	bad8 := def
	bad8.Tiers.MaxTier = 1
	rec(t, "ValidateConfig", []any{toConfigJSON(bad8)}, noa.ValidateConfig(bad8))

	bad9 := def
	bad9.Compress.MinSummaryLength = 20000
	bad9.Compress.MaxSummaryLength = 20000
	rec(t, "ValidateConfig", []any{toConfigJSON(bad9)}, noa.ValidateConfig(bad9))

	negBenefit := -5
	bad10 := def
	bad10.Nudge.MinPressureBenefitTokens = &negBenefit
	rec(t, "ValidateConfig", []any{toConfigJSON(bad10)}, noa.ValidateConfig(bad10))

	posBenefit := 0
	okBenefit := def
	okBenefit.Nudge.MinPressureBenefitTokens = &posBenefit
	rec(t, "ValidateConfig", []any{toConfigJSON(okBenefit)}, noa.ValidateConfig(okBenefit))

	bad11 := def
	bad11.MaxCompressAttempts = 0
	rec(t, "ValidateConfig", []any{toConfigJSON(bad11)}, noa.ValidateConfig(bad11))

	// A kitchen-sink config tripping many warnings at once.
	sink := noa.Config{
		ModelContextLimit:      -1,
		PreserveRecentMessages: 5,
		MaxCompressAttempts:    0,
	}
	sink.Tiers.Tier2Trigger = 0
	sink.Tiers.Tier3Trigger = 0
	sink.Tiers.MaxTier = 5
	sink.Nudge.MinContextLimitPct = 0.9
	sink.Nudge.MaxContextLimitPct = 0.5
	sink.Nudge.EmergencyThresholdPct = 0.3
	sink.Truncate.Threshold = 2
	sink.Compress.MinSummaryLength = 10
	sink.Compress.MaxSummaryLength = 5
	rec(t, "ValidateConfig", []any{toConfigJSON(sink)}, noa.ValidateConfig(sink))
}

// msg is a CoreMessage fixture helper.
func msg(id, role, ct, text, tname, tcid string) noa.CoreMessage {
	return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text, ToolName: tname, ToolCallID: tcid}
}

func TestZZOracle9c(t *testing.T) {
	// A conversation exercising: reasoning run, act (tool-call), result, plain act,
	// user text, a second turn with reasoning + text act + no result.
	base := []noa.CoreMessage{
		msg("a1", "assistant", "reasoning", "think", "", ""),
		msg("a2", "assistant", "tool-call", "", "Bash", "c1"),
		msg("t1", "tool", "tool-result", "out", "", "c1"),
		msg("u1", "user", "text", "hi", "", ""),
		msg("a3", "assistant", "text", "reply", "", ""),
	}

	// ---- ComputeTurnGroups ----
	fixtures := map[string][]noa.CoreMessage{
		"base":        base,
		"empty":       {},
		"single_act":  {msg("a1", "assistant", "text", "x", "", "")},
		"single_res":  {msg("r1", "assistant", "reasoning", "t", "", "")},
		"no_reasoning_act": {
			msg("a1", "assistant", "tool-call", "", "Read", "c1"),
			msg("t1", "tool", "tool-result", "o", "", "c1"),
			msg("a2", "assistant", "tool-call", "", "Bash", "c2"),
			msg("t2", "tool", "tool-result", "o2", "", "c2"),
		},
		"reasoning_then_user": {
			msg("a1", "assistant", "reasoning", "t", "", ""),
			msg("u1", "user", "text", "hi", "", ""),
		},
		"unmatched_result": {
			msg("a1", "assistant", "tool-call", "", "Bash", "c1"),
			msg("t1", "tool", "tool-result", "orphan", "", "cX"), // callID mismatch -> not in group
			msg("a2", "assistant", "text", "done", "", ""),
		},
		"multi_reasoning": {
			msg("a1", "assistant", "reasoning", "r1", "", ""),
			msg("a2", "assistant", "reasoning", "r2", "", ""),
			msg("a3", "assistant", "text", "act", "", ""),
		},
	}
	// stable order for the map iteration
	order := []string{"base", "empty", "single_act", "single_res", "no_reasoning_act", "reasoning_then_user", "unmatched_result", "multi_reasoning"}
	for _, name := range order {
		rec(t, "ComputeTurnGroups", []any{fixtures[name]}, noa.ComputeTurnGroups(fixtures[name]))
	}

	// ---- WithdrawSplitTurns: fold reasoning (and variants) over base + extras ----
	rec(t, "WithdrawSplitTurns", []any{base, map[string]bool{"a1": true}}, withdrawOut(noa.WithdrawSplitTurns(base, map[string]bool{"a1": true})))
	rec(t, "WithdrawSplitTurns", []any{base, map[string]bool{}}, withdrawOut(noa.WithdrawSplitTurns(base, map[string]bool{})))
	rec(t, "WithdrawSplitTurns", []any{base, nil}, withdrawOut(noa.WithdrawSplitTurns(base, nil)))
	// fold reasoning + result together -> nothing withdrawn (reasoning gone, no visible call)
	rec(t, "WithdrawSplitTurns", []any{base, map[string]bool{"a1": true, "t1": true}}, withdrawOut(noa.WithdrawSplitTurns(base, map[string]bool{"a1": true, "t1": true})))
	// fold only a result, reasoning stays -> nothing to withdraw
	rec(t, "WithdrawSplitTurns", []any{base, map[string]bool{"t1": true}}, withdrawOut(noa.WithdrawSplitTurns(base, map[string]bool{"t1": true})))
	// fold act text (a3) only -> no reasoning involved
	rec(t, "WithdrawSplitTurns", []any{base, map[string]bool{"a3": true}}, withdrawOut(noa.WithdrawSplitTurns(base, map[string]bool{"a3": true})))
	// multi_reasoning: fold one reasoning of two -> split turn withdrawn
	mr := fixtures["multi_reasoning"]
	rec(t, "WithdrawSplitTurns", []any{mr, map[string]bool{"a1": true}}, withdrawOut(noa.WithdrawSplitTurns(mr, map[string]bool{"a1": true})))
}

// withdrawOut shapes the (map, slice) pair the way the oracle records it: a
// two-element array [out, withdrawn]. A nil map renders as null, an empty map as {}.
func withdrawOut(out map[string]bool, withdrawn []string) []any {
	return []any{out, withdrawn}
}

// pair returns a (start, end) pair as a two-element array.
func pair(start, end int) []any { return []any{start, end} }

func TestZZOracle9d(t *testing.T) {
	// A conversation with tool exchanges and reasoning runs, used to exercise the
	// pair-boundary adjusters. Indices are stable.
	//
	//  0 u1  user text
	//  1 a1  assistant reasoning
	//  2 a2  assistant tool-call Bash(c1)
	//  3 t1  tool tool-result (c1)
	//  4 u2  user text
	//  5 a3  assistant reasoning
	//  6 a4  assistant tool-call Read(c2)
	//  7 t2  tool tool-result (c2)
	//  8 a5  assistant text
	msgs := []noa.CoreMessage{
		msg("u1", "user", "text", "hi", "", ""),
		msg("a1", "assistant", "reasoning", "t", "", ""),
		msg("a2", "assistant", "tool-call", "", "Bash", "c1"),
		msg("t1", "tool", "tool-result", "o", "", "c1"),
		msg("u2", "user", "text", "yo", "", ""),
		msg("a3", "assistant", "reasoning", "t", "", ""),
		msg("a4", "assistant", "tool-call", "", "Read", "c2"),
		msg("t2", "tool", "tool-result", "o2", "", "c2"),
		msg("a5", "assistant", "text", "done", "", ""),
	}

	// ---- AdjustBoundariesForReasoningPairs ----
	// tail reasoning pulls in the following assistant burst
	rec(t, "AdjustBoundariesForReasoningPairs", []any{5, 5, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(5, 5, msgs)))
	// head assistant act pulls in the reasoning run before it
	rec(t, "AdjustBoundariesForReasoningPairs", []any{6, 6, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(6, 6, msgs)))
	// range that already covers reasoning+act: no change
	rec(t, "AdjustBoundariesForReasoningPairs", []any{5, 6, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(5, 6, msgs)))
	// invalid bounds
	rec(t, "AdjustBoundariesForReasoningPairs", []any{-1, 2, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(-1, 2, msgs)))
	rec(t, "AdjustBoundariesForReasoningPairs", []any{0, 99, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(0, 99, msgs)))
	rec(t, "AdjustBoundariesForReasoningPairs", []any{3, 2, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(3, 2, msgs)))
	// range starting at plain user text: no reasoning involved
	rec(t, "AdjustBoundariesForReasoningPairs", []any{0, 0, msgs}, pair(noa.AdjustBoundariesForReasoningPairs(0, 0, msgs)))

	// ---- AdjustBoundariesForToolPairs ----
	// take the tool-call (idx 2) without its result (idx 3) -> pulls result forward
	rec(t, "AdjustBoundariesForToolPairs", []any{2, 2, msgs, 20}, pair(noa.AdjustBoundariesForToolPairs(2, 2, msgs, 20)))
	// take the result (idx 3) without its call (idx 2) -> pulls call backward
	rec(t, "AdjustBoundariesForToolPairs", []any{3, 3, msgs, 20}, pair(noa.AdjustBoundariesForToolPairs(3, 3, msgs, 20)))
	// both call+result already in range -> no change
	rec(t, "AdjustBoundariesForToolPairs", []any{2, 3, msgs, 20}, pair(noa.AdjustBoundariesForToolPairs(2, 3, msgs, 20)))
	// maxScan=0 -> no scan, stays put even though pair is split
	rec(t, "AdjustBoundariesForToolPairs", []any{2, 2, msgs, 0}, pair(noa.AdjustBoundariesForToolPairs(2, 2, msgs, 0)))
	// scan hits a non-matching message and stops (idx 4 user, not in range)
	rec(t, "AdjustBoundariesForToolPairs", []any{3, 3, msgs, 20}, pair(noa.AdjustBoundariesForToolPairs(3, 3, msgs, 20)))
	// range with no tool-call ids -> early return
	rec(t, "AdjustBoundariesForToolPairs", []any{0, 0, msgs, 20}, pair(noa.AdjustBoundariesForToolPairs(0, 0, msgs, 20)))
	// Compress tool is excluded from dragging boundaries
	compressMsgs := []noa.CoreMessage{
		msg("u1", "user", "text", "hi", "", ""),
		msg("c1", "assistant", "tool-call", "", noa.CompressToolName, "cc1"),
		msg("cr1", "tool", "tool-result", "summary", "", "cc1"),
	}
	rec(t, "AdjustBoundariesForToolPairs", []any{1, 1, compressMsgs, 20}, pair(noa.AdjustBoundariesForToolPairs(1, 1, compressMsgs, 20)))

	// ---- ApplyPairBoundaryAdjustments (fixed point, <=2 passes) ----
	// reasoning at 5 + tool-call at 6 split -> both adjusters interact
	rec(t, "ApplyPairBoundaryAdjustments", []any{5, 5, msgs}, pair(noa.ApplyPairBoundaryAdjustments(5, 5, msgs)))
	rec(t, "ApplyPairBoundaryAdjustments", []any{6, 6, msgs}, pair(noa.ApplyPairBoundaryAdjustments(6, 6, msgs)))
	rec(t, "ApplyPairBoundaryAdjustments", []any{2, 2, msgs}, pair(noa.ApplyPairBoundaryAdjustments(2, 2, msgs)))
	rec(t, "ApplyPairBoundaryAdjustments", []any{3, 3, msgs}, pair(noa.ApplyPairBoundaryAdjustments(3, 3, msgs)))
	rec(t, "ApplyPairBoundaryAdjustments", []any{0, 8, msgs}, pair(noa.ApplyPairBoundaryAdjustments(0, 8, msgs)))
	rec(t, "ApplyPairBoundaryAdjustments", []any{0, 0, msgs}, pair(noa.ApplyPairBoundaryAdjustments(0, 0, msgs)))
	rec(t, "ApplyPairBoundaryAdjustments", []any{-1, 99, msgs}, pair(noa.ApplyPairBoundaryAdjustments(-1, 99, msgs)))
}
func TestZZOracle9e(t *testing.T) {
	// ParseCompressArgs: a spread of raw inputs exercising every parse layer and
	// every CompressParseKind. Raw is passed as a string (recorded as text).
	inputs := []string{
		// empty / null
		"", "   ", "null",
		// not an object
		`[1,2,3]`, `"just a string"`, `42`,
		// missing content
		`{"topic":"x"}`,
		// content not an array (object)
		`{"content":{"a":1}}`,
		// content not an array (number)
		`{"content":5}`,
		// empty content list -> ok, no ranges
		`{"content":[]}`,
		// well-formed
		`{"topic":"t","content":[{"startId":"m00001","endId":"m00005","summary":"sum","topic":"e1"},{"startRef":"m00006","endRef":"m00010","summary":"sum2"}]}`,
		// summaryMaxChars at top + per-entry
		`{"summaryMaxChars":1234,"content":[{"messageId":"m00001","endRef":"m00003","summary":"s","summaryMaxChars":99}]}`,
		// field aliases
		`{"content":[{"startId":"m1","endId":"m2","summary":"a"},{"startRef":"m3","endRef":"m4","summary":"b"}]}`,
		// missing fields -> invalid items -> no-valid-ranges
		`{"content":[{"topic":"x"},{"summary":"only summary"}]}`,
		// mixed valid + invalid
		`{"content":[{"startId":"m1","endId":"m2","summary":"ok"},{"startId":"","endId":"","summary":""}]}`,
		// content as JSON-string of array
		`{"content":"[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"s\"}]"}`,
		// markdown fence wrapper
		"```json\n{\"content\":[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"s\"}]}\n```",
		"```\n{\"content\":[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"s\"}]}\n```",
		// whole args as a JSON string
		`"{\"content\":[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"s\"}]}"`,
		// trailing comma
		`{"content":[{"startId":"m1","endId":"m2","summary":"s"},],}`,
		// raw newline inside a summary string
		"{\"content\":[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"line1\nline2\"}]}",
		// tail repair: missing closing brace on last object
		`{"content":[{"startId":"m1","endId":"m2","summary":"s"}`,
		// salvage: truncated mid-object (provider cut off)
		`{"content":[{"startId":"m1","endId":"m2","summary":"s"},{"startId":"m3","endId":"m4","summ`,
		// malformed / unrecoverable
		`{not json at all`,
		// leading/trailing whitespace
		"  \n\t {\"content\":[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"s\"}]} \n ",
		// topic omitted -> empty
		`{"content":[{"startId":"m1","endId":"m2","summary":"s"}]}`,
		// unicode + special chars in summary
		`{"content":[{"startId":"m1","endId":"m2","summary":"héllo 世界 😀"}]}`,
		// multiple ranges
		`{"content":[{"startId":"m1","endId":"m2","summary":"a"},{"startId":"m3","endId":"m4","summary":"b"},{"startId":"m5","endId":"m6","summary":"c"}]}`,
		// tail repair (Qwen-style): content is a JSON-string array whose last object
		// lost its closing brace, leaving `"]` where `"}]` belongs. Raw string keeps
		// the \" literal so the outer content value carries the backslashes.
		`{"content":"[{\"startId\":\"m1\",\"endId\":\"m2\",\"summary\":\"s\"]"}`,
		// double-stringified payload
		`"{"content":[{"startId":"m1","endId":"m2","summary":"s"}]}"`,
	}
	for _, raw := range inputs {
		res := noa.ParseCompressArgs([]byte(raw))
		// Keys is filled from a Go map range (random order); sort it so the recorded
		// case is byte-reproducible. The Rust port emits serde_json's sorted keys.
		sort.Strings(res.Diagnostics.Keys)
		rec(t, "ParseCompressArgs", []any{raw}, res)
	}
}

// bdArg is the subset of NudgeDecision that FormatBreakdown reads.
type bdArg struct {
	ContextBreakdown map[string]int
	Growth           int
}

// cbArg is the subset of CompressionBlock that FormatTierTargetBlocks reads.
type cbArg struct {
	BlockID             string
	Tier                int
	Topic               string
	Summary             string
	EffectiveMessageIDs []string
	CompressedTokens    int
}

func TestZZOracle9f(t *testing.T) {
	// ---- FormatBreakdown ----
	breakdowns := []bdArg{
		{ContextBreakdown: map[string]int{"system": 1200, "tool": 3400, "code": 500, "text": 800}, Growth: 0},
		{ContextBreakdown: map[string]int{"system": 1200, "summaries": 900}, Growth: 2500},
		{ContextBreakdown: map[string]int{}, Growth: 0},
		{ContextBreakdown: map[string]int{"text": 0, "tool": 0}, Growth: 100},
		{ContextBreakdown: map[string]int{"code": 999999}, Growth: 12345},
		{ContextBreakdown: map[string]int{"unknownbucket": 5000, "system": 100}, Growth: 0},
	}
	for _, b := range breakdowns {
		d := noa.NudgeDecision{ContextBreakdown: b.ContextBreakdown}
		d.Breakdown.Growth = b.Growth
		rec(t, "FormatBreakdown", []any{b}, noa.FormatBreakdown(d))
	}

	// ---- FormatBlockMap ----
	spanSets := map[string][]noa.BlockSpan{
		"empty":   {},
		"one":     {{BlockID: "b5", Tier: 1, StartRef: "m00001", EndRef: "m00010"}},
		"three":   {{BlockID: "b1", Tier: 1, StartRef: "m00001", EndRef: "m00005"}, {BlockID: "b2", Tier: 2, StartRef: "m00006", EndRef: "m00020"}, {BlockID: "b3", Tier: 3, StartRef: "m00001", EndRef: "m00020"}},
		"ten": {
			{BlockID: "b1", Tier: 1, StartRef: "m00001", EndRef: "m00002"},
			{BlockID: "b2", Tier: 1, StartRef: "m00003", EndRef: "m00004"},
			{BlockID: "b3", Tier: 1, StartRef: "m00005", EndRef: "m00006"},
			{BlockID: "b4", Tier: 1, StartRef: "m00007", EndRef: "m00008"},
			{BlockID: "b5", Tier: 1, StartRef: "m00009", EndRef: "m00010"},
			{BlockID: "b6", Tier: 2, StartRef: "m00011", EndRef: "m00015"},
			{BlockID: "b7", Tier: 2, StartRef: "m00016", EndRef: "m00020"},
			{BlockID: "b8", Tier: 2, StartRef: "m00021", EndRef: "m00025"},
			{BlockID: "b9", Tier: 3, StartRef: "m00001", EndRef: "m00025"},
			{BlockID: "b10", Tier: 3, StartRef: "m00001", EndRef: "m00025"},
		},
	}
	order := []string{"empty", "one", "three", "ten"}
	for _, name := range order {
		rec(t, "FormatBlockMap", []any{spanSets[name]}, noa.FormatBlockMap(spanSets[name]))
	}

	// ---- FormatTierTargetBlocks ----
	blockSets := map[string][]cbArg{
		"empty": {},
		"one":   {{BlockID: "b1", Tier: 1, Topic: "auth flow", Summary: "did the auth thing", EffectiveMessageIDs: []string{"a", "b", "c"}, CompressedTokens: 12000}},
		"two": {
			{BlockID: "b1", Tier: 2, Topic: "", Summary: "short", EffectiveMessageIDs: []string{"a", "b"}, CompressedTokens: 5000},
			{BlockID: "b2", Tier: 2, Topic: "db work", Summary: "a longer summary here", EffectiveMessageIDs: []string{"c", "d", "e", "f", "g"}, CompressedTokens: 250000},
		},
		"noTopic": {{BlockID: "b9", Tier: 3, Topic: "", Summary: "x", EffectiveMessageIDs: nil, CompressedTokens: 0}},
	}
	border := []string{"empty", "one", "two", "noTopic"}
	for _, name := range border {
		cbs := blockSets[name]
		var blocks []noa.CompressionBlock
		for _, c := range cbs {
			blocks = append(blocks, noa.CompressionBlock{
				BlockID:             c.BlockID,
				Tier:                noa.Tier(c.Tier),
				Topic:               c.Topic,
				Summary:             c.Summary,
				EffectiveMessageIDs: c.EffectiveMessageIDs,
				CompressedTokens:    c.CompressedTokens,
			})
		}
		rec(t, "FormatTierTargetBlocks", []any{cbs}, noa.FormatTierTargetBlocks(blocks))
	}
}

func TestZZOracle9g(t *testing.T) {
	// ---- MergeRangesToThreshold ----
	type rg struct {
		start, end                     string
		count, tokens, chars           int
		toolPct, textPct, userMsgs     int
		dangerous                      bool
	}
	mk := func(r rg) noa.RangeInfo {
		return noa.RangeInfo{
			StartRef: r.start, EndRef: r.end, Count: r.count, Tokens: r.tokens,
			Chars: r.chars, ToolPct: r.toolPct, TextPct: r.textPct,
			UserMsgs: r.userMsgs, Dangerous: r.dangerous,
		}
	}
	mergeCases := []struct {
		ranges   []rg
		minChars int
	}{
		// below threshold each -> all merged into one
		{[]rg{{"m00001", "m00005", 5, 1000, 100, 60, 40, 1, false}, {"m00006", "m00010", 5, 2000, 150, 30, 70, 2, false}}, 500},
		// first already above threshold -> flush each
		{[]rg{{"m00001", "m00005", 5, 1000, 600, 60, 40, 1, false}, {"m00006", "m00010", 5, 2000, 150, 30, 70, 2, false}}, 500},
		// exactly at threshold
		{[]rg{{"m00001", "m00005", 5, 1000, 500, 60, 40, 1, false}}, 500},
		// empty
		{[]rg{}, 500},
		// minChars <= 0 -> unchanged
		{[]rg{{"m00001", "m00005", 5, 1000, 100, 60, 40, 1, false}}, 0},
		// three small -> one merged, weighted tool pct
		{[]rg{{"m1", "m2", 2, 100, 50, 100, 0, 0, false}, {"m3", "m4", 3, 150, 60, 0, 100, 1, false}, {"m5", "m6", 1, 80, 40, 50, 50, 0, false}}, 1000},
	}
	for _, c := range mergeCases {
		var rs []noa.RangeInfo
		for _, r := range c.ranges {
			rs = append(rs, mk(r))
		}
		out := noa.MergeRangesToThreshold(rs, c.minChars)
		rec(t, "MergeRangesToThreshold", []any{rs, c.minChars}, out)
	}

	// ---- ViableRanges ----
	viableCases := [][]rg{
		{{"m00001", "m00005", 5, 100, 100, 60, 40, 1, false}},   // below 200 -> dropped
		{{"m00001", "m00005", 5, 200, 100, 60, 40, 1, false}},   // exactly 200 -> kept
		{{"m00001", "m00005", 5, 500, 100, 60, 40, 1, false}},   // above -> kept
		{{"m1", "m2", 1, 50, 10, 0, 0, 0, false}, {"m3", "m4", 1, 250, 20, 0, 0, 0, false}}, // mixed
		{},                                                       // empty
	}
	for _, c := range viableCases {
		var rs []noa.RangeInfo
		for _, r := range c {
			rs = append(rs, mk(r))
		}
		out := noa.ViableRanges(rs)
		rec(t, "ViableRanges", []any{rs}, out)
	}

	// ---- CompressToolSchema ----
	rec(t, "CompressToolSchema", []any{}, noa.CompressToolSchema())
}

func TestZZOracle9h(t *testing.T) {
	// ---- ArchiveTag ----
	type blk struct {
		ID, Start, End, Path string
		Tier                 int
	}
	blocks := []blk{
		{"b5", "m00001", "m00010", "/arc/b5.md", 1},
		{"b12", "m00001", "m00050", "/arc/tier2/b12.md", 2},
		{"b3", "m00100", "m00200", "rel/path/b3.md", 3},
	}
	for _, b := range blocks {
		cb := noa.CompressionBlock{
			BlockID: b.ID, Tier: noa.Tier(b.Tier),
			StartRef: b.Start, EndRef: b.End, ArchivePath: b.Path,
		}
		rec(t, "ArchiveTag", []any{b}, noa.ArchiveTag(cb))
	}

	// ---- MessageRefMap helpers ----
	mkmap := func(byRaw, byRef map[string]string) noa.MessageRefMap {
		return noa.MessageRefMap{ByRaw: byRaw, ByRef: byRef}
	}
	maps := []struct {
		ByRaw, ByRef map[string]string
	}{
		{
			map[string]string{"h1": "m00001", "h2": "m00002", "h3": "BLOCKED"},
			map[string]string{"m00001": "h1", "m00002": "h2", "BLOCKED": "h3"},
		},
		{
			map[string]string{"h1": "m00007", "h2": "m00042", "h3": "m00003"},
			map[string]string{"m00007": "h1", "m00042": "h2", "m00003": "h3"},
		},
		{
			map[string]string{},
			map[string]string{},
		},
	}
	for _, mp := range maps {
		m := mkmap(mp.ByRaw, mp.ByRef)
		// HighestUsedIndex
		rec(t, "HighestUsedIndex", []any{mp.ByRaw}, noa.HighestUsedIndex(m))
		// RefForRaw / RawForRef for a few probes
		probes := []string{"h1", "h2", "h3", "hX"}
		for _, p := range probes {
			r, ok := noa.RefForRaw(m, p)
			rec(t, "RefForRaw", []any{mp.ByRaw, p}, map[string]any{"ref": r, "ok": ok})
		}
		refprobes := []string{"m00001", "m00007", "m00042", "BLOCKED", "m99999"}
		for _, p := range refprobes {
			r, ok := noa.RawForRef(m, p)
			rec(t, "RawForRef", []any{mp.ByRef, p}, map[string]any{"raw": r, "ok": ok})
		}
	}

	// ---- MatchToolPattern ----
	patterns := [][2]string{
		{"", "Bash"},
		{"Bash", ""},
		{"bash", "BASH"},
		{"Bash", "Bash"},
		{"Read", "Read"},
		{"Read", "Write"},
		{"mcp_*", "mcp__github"},
		{"mcp_*", "mcp_x"},
		{"mcp_*", "MCP_X"},
		{"*", "anything"},
		{"*", ""},
		{"Web*", "WebFetch"},
		{"Web*", "websearch"},
		{"exact", "exactly"},
		{"exact*", "exact"},
	}
	for _, p := range patterns {
		rec(t, "MatchToolPattern", []any{p[0], p[1]}, noa.MatchToolPattern(p[0], p[1]))
	}

	// ---- IsNeverPreserveRecentTool ----
	tools := []string{"", "Read", "read", "Grep", "Glob", "Bash", "LS", "ls", "Write", "Edit", "WebFetch", "Compress"}
	for _, tn := range tools {
		m := noa.CoreMessage{ToolName: tn}
		rec(t, "IsNeverPreserveRecentTool", []any{tn}, noa.IsNeverPreserveRecentTool(m))
	}

	// ---- IsMessageProtected (host predicate modeled as resolved set) ----
	type ipCase struct {
		tool         string
		protected    []string
		hostProtected []string // nil => no host predicate
	}
	ipCases := []ipCase{
		{"", nil, nil},
		{"Bash", nil, nil},
		{"Compress", nil, nil},                              // AlwaysProtectedTools
		{"compress", nil, nil},                              // case-sensitive: not protected
		{"Read", []string{"Read"}, nil},                     // exact via pattern
		{"Read", []string{"read"}, nil},                     // pattern case-insensitive
		{"mcp__x", []string{"mcp_*"}, nil},                  // wildcard
		{"WebFetch", []string{"mcp_*"}, nil},                // no match
		{"Write", nil, []string{"Write"}},                   // host predicate
		{"Write", nil, []string{"Read"}},                    // host predicate miss
		{"Custom1", []string{"Other"}, []string{"Custom1"}}, // pattern miss + host hit
		{"Custom2", []string{"Custom*"}, nil},               // pattern hit
	}
	for _, c := range ipCases {
		cfg := noa.Config{ProtectedTools: c.protected}
		if c.hostProtected != nil {
			set := make(map[string]bool)
			for _, h := range c.hostProtected {
				set[h] = true
			}
			cfg.IsToolProtected = func(name string) bool { return set[name] }
		}
		m := noa.CoreMessage{ToolName: c.tool}
		got := noa.IsMessageProtected(m, cfg)
		// record args as the resolved view
		arg := map[string]any{
			"ToolName":       c.tool,
			"ProtectedTools": c.protected,
			"HostProtected":  c.hostProtected,
		}
		rec(t, "IsMessageProtected", []any{arg}, got)
	}
}

// sumArg is the subset of CompressionBlock that RenderSummaryMessage reads.
type sumArg struct {
	ID, Topic, Summary, Start, End, Path string
	Tier                                 int
}

func TestZZOracle9i(t *testing.T) {
	cases := []sumArg{
		// full: topic + summary
		{"b5", "auth flow", "Resumed the auth refactor.", "m00001", "m00010", "/arc/b5.md", 1},
		// no topic
		{"b6", "", "Just a summary with no topic.", "m00011", "m00020", "/arc/b6.md", 1},
		// topic but empty summary
		{"b7", "db migration", "", "m00021", "m00030", "/arc/b7.md", 2},
		// both empty
		{"b8", "", "", "m00031", "m00040", "/arc/b8.md", 2},
		// summary with leading/trailing whitespace -> TrimSpace
		{"b9", "  spaced topic  ", "   \n trimmed body \t ", "m00041", "m00050", "/arc/b9.md", 3},
		// unicode topic + summary
		{"b12", "café ☕ 世界", "résumé of the 世界 section 😀", "m00001", "m00100", "/arc/tier2/b12.md", 3},
		// tier 1 with special chars in summary
		{"b1", "quotes \" and \\ backslash", "line with \"quotes\" and path C:\\x", "m00001", "m00005", "/a b/c.md", 1},
	}
	for _, c := range cases {
		cb := noa.CompressionBlock{
			BlockID: c.ID, Tier: noa.Tier(c.Tier), Topic: c.Topic, Summary: c.Summary,
			StartRef: c.Start, EndRef: c.End, ArchivePath: c.Path,
		}
		rec(t, "RenderSummaryMessage", []any{c}, noa.RenderSummaryMessage(cb))
	}
}

func TestZZOracle9j(t *testing.T) {
	type rg struct {
		start, end                 string
		count, tokens, userMsgs    int
		toolPct, textPct           int
		dangerous                  bool
	}
	type pr struct {
		start, end        string
		count, tokens     int
		tools             []string
	}
	mkR := func(r rg) noa.RangeInfo {
		return noa.RangeInfo{StartRef: r.start, EndRef: r.end, Count: r.count, Tokens: r.tokens,
			UserMsgs: r.userMsgs, ToolPct: r.toolPct, TextPct: r.textPct, Dangerous: r.dangerous}
	}
	mkP := func(p pr) noa.ProtectedRange {
		return noa.ProtectedRange{StartRef: p.start, EndRef: p.end, Count: p.count, Tokens: p.tokens, Tools: p.tools}
	}
	type fc struct {
		comp []rg
		prot []pr
	}
	cases := []fc{
		// empty both
		{nil, nil},
		// single compressible
		{[]rg{{"m00001", "m00010", 10, 4000, 0, 60, 40, false}}, nil},
		// single protected
		{nil, []pr{{"m00001", "m00005", 5, 2000, []string{"Write", "Edit"}}}},
		// pure protected, multiple tools
		{[]rg{}, []pr{{"m00020", "m00030", 11, 5000, []string{"Bash", "Read"}}}},
		// mixed overlapping (compressible + protected same span -> merge)
		{[]rg{{"m00001", "m00010", 10, 4000, 0, 60, 40, false}},
			[]pr{{"m00001", "m00010", 10, 2000, []string{"Write"}}}},
		// dangerous compressible
		{[]rg{{"m00001", "m00008", 8, 3000, 2, 30, 70, true}}, nil},
		// dangerous but no compressible tokens (protected) -> no suffix
		{nil, []pr{{"m00001", "m00005", 5, 2000, []string{"Write"}}}},
		// adjacent merge (endNum+1) -> two compressible merge
		{[]rg{{"m00001", "m00005", 5, 1000, 0, 60, 40, false}, {"m00006", "m00010", 5, 1500, 1, 30, 70, false}}, nil},
		// gap (no merge)
		{[]rg{{"m00001", "m00005", 5, 1000, 0, 60, 40, false}, {"m00010", "m00015", 6, 2000, 3, 30, 70, false}}, nil},
		// compressible + adjacent protected -> merge into mixed
		{[]rg{{"m00001", "m00005", 5, 1000, 1, 60, 40, false}},
			[]pr{{"m00006", "m00010", 5, 1500, []string{"Bash"}}}},
		// unsorted input (sort by startNum) - protected first then compressible
		{[]rg{{"m00010", "m00020", 10, 3000, 0, 50, 50, false}},
			[]pr{{"m00001", "m00005", 5, 1000, []string{"Read"}}}},
		// user_msgs plural note
		{[]rg{{"m00001", "m00020", 20, 6000, 3, 40, 60, false}}, nil},
		// three-way merge
		{[]rg{{"m00001", "m00004", 4, 800, 0, 60, 40, false},
			{"m00005", "m00008", 4, 900, 0, 30, 70, false},
			{"m00009", "m00012", 4, 1000, 1, 50, 50, false}}, nil},
	}
	for _, c := range cases {
		var comp []noa.RangeInfo
		for _, r := range c.comp {
			comp = append(comp, mkR(r))
		}
		var prot []noa.ProtectedRange
		for _, p := range c.prot {
			prot = append(prot, mkP(p))
		}
		rec(t, "FormatRanges", []any{comp, prot}, noa.FormatRanges(comp, prot))
	}
}

func TestZZOracle9k(t *testing.T) {
	mkBlk := func(id string, tier int, start, end, path string, eff []string) noa.CompressionBlock {
		return noa.CompressionBlock{
			BlockID: id, Tier: noa.Tier(tier), StartRef: start, EndRef: end,
			ArchivePath: path, EffectiveMessageIDs: eff,
		}
	}
	mkState := func(blocks []noa.CompressionBlock, byRef map[string]string) noa.CompressionState {
		if byRef == nil {
			byRef = map[string]string{}
		}
		return noa.CompressionState{
			Blocks:      blocks,
			MessageRefs: noa.MessageRefMap{ByRaw: map[string]string{}, ByRef: byRef},
		}
	}
	type pc struct {
		name           string
		before, after  int
		blocks         []noa.CompressionBlock
		errors, warns  []string
		surviving      []string
		state          noa.CompressionState
	}
	cases := []pc{
		{
			name: "empty", before: 5000, after: 5000,
			state: mkState(nil, nil),
		},
		{
			name: "no-blocks-errors-warns", before: 5000, after: 5000,
			errors: []string{"bad ref m99999", "overlap"}, warns: []string{"summary short"},
			state: mkState(nil, nil),
		},
		{
			name: "one-block-clean", before: 8000, after: 3000,
			blocks: []noa.CompressionBlock{mkBlk("b10", 3, "m00001", "m00005", "/arc/b10.md", []string{"h1", "h2", "h3", "h4", "h5"})},
			state: mkState(
				[]noa.CompressionBlock{mkBlk("b10", 3, "m00001", "m00005", "/arc/b10.md", nil)},
				map[string]string{"m00001": "h1", "m00002": "h2", "m00003": "h3", "m00004": "h4", "m00005": "h5"},
			),
		},
		{
			name: "reclaimed-negative", before: 2000, after: 5000,
			blocks: []noa.CompressionBlock{mkBlk("b10", 1, "m00001", "m00003", "/arc/b10.md", []string{"h1", "h2", "h3"})},
			state: mkState(
				[]noa.CompressionBlock{mkBlk("b10", 1, "m00001", "m00003", "/arc/b10.md", nil)},
				map[string]string{"m00001": "h1", "m00002": "h2", "m00003": "h3"},
			),
		},
		{
			name: "surviving-tier", before: 9000, after: 4000,
			blocks:    []noa.CompressionBlock{mkBlk("b10", 3, "m00001", "m00020", "/arc/b10.md", []string{"h1"})},
			surviving: []string{"b5"},
			state: mkState(
				[]noa.CompressionBlock{mkBlk("b5", 2, "m00001", "m00010", "/arc/b5.md", nil), mkBlk("b10", 3, "m00001", "m00020", "/arc/b10.md", nil)},
				map[string]string{"m00001": "h1"},
			),
		},
		{
			name: "surviving-missing-id", before: 9000, after: 4000,
			blocks:    []noa.CompressionBlock{mkBlk("b10", 2, "m00001", "m00010", "/arc/b10.md", []string{"h1"})},
			surviving: []string{"b99"},
			state:     mkState([]noa.CompressionBlock{mkBlk("b10", 2, "m00001", "m00010", "/arc/b10.md", nil)}, map[string]string{"m00001": "h1"}),
		},
		{
			name: "uncovered-star", before: 8000, after: 2000,
			blocks: []noa.CompressionBlock{mkBlk("b10", 1, "m00001", "m00005", "/arc/b10.md", []string{"h1", "h2", "h3", "h5"})},
			state: mkState(
				[]noa.CompressionBlock{mkBlk("b10", 1, "m00001", "m00005", "/arc/b10.md", nil)},
				map[string]string{"m00001": "h1", "m00002": "h2", "m00003": "h3", "m00004": "h4", "m00005": "h5"},
			),
		},
		{
			name: "two-blocks", before: 12000, after: 4000,
			blocks: []noa.CompressionBlock{
				mkBlk("b10", 1, "m00001", "m00005", "/arc/b10.md", []string{"h1", "h2"}),
				mkBlk("b11", 2, "m00006", "m00010", "/arc/b11.md", []string{"h3", "h4"}),
			},
			errors: []string{"e1"}, warns: []string{"w1", "w2"},
			state: mkState(
				[]noa.CompressionBlock{mkBlk("b10", 1, "m00001", "m00005", "/arc/b10.md", nil), mkBlk("b11", 2, "m00006", "m00010", "/arc/b11.md", nil)},
				map[string]string{"m00001": "h1", "m00002": "h2", "m00006": "h3", "m00007": "h4"},
			),
		},
		{
			name: "bad-refs-no-star", before: 5000, after: 1000,
			blocks: []noa.CompressionBlock{mkBlk("b10", 1, "x", "y", "/arc/b10.md", []string{"h1"})},
			state:  mkState([]noa.CompressionBlock{mkBlk("b10", 1, "x", "y", "/arc/b10.md", nil)}, map[string]string{"m00001": "h2"}),
		},
	}
	for _, c := range cases {
		in := noa.PanelInput{
			Result: noa.ApplyResult{
				State:             c.state,
				BlocksCreated:     c.blocks,
				Errors:            c.errors,
				Warnings:          c.warns,
				SurvivingBlockIDs: c.surviving,
			},
			BeforeTokens: c.before,
			AfterTokens:  c.after,
		}
		rec(t, "FormatPanel", []any{c.name, c.before, c.after, c.blocks, c.surviving, c.errors, c.warns, c.state}, noa.FormatPanel(in))
	}
}

// blkOut is the observable subset of CompressionBlock for ActiveBlocks output.
type blkOut struct {
	BlockID             string
	Tier                int
	StartRef            string
	EndRef              string
	Active              bool
	EffectiveMessageIDs []string
}

func TestZZOracle9l(t *testing.T) {
	mkBlk := func(id string, tier int, start, end string, active bool, eff []string) noa.CompressionBlock {
		return noa.CompressionBlock{
			BlockID: id, Tier: noa.Tier(tier), StartRef: start, EndRef: end,
			Active: active, EffectiveMessageIDs: eff,
		}
	}
	toOut := func(bs []noa.CompressionBlock) []blkOut {
		out := make([]blkOut, 0, len(bs))
		for _, b := range bs {
			out = append(out, blkOut{b.BlockID, int(b.Tier), b.StartRef, b.EndRef, b.Active, b.EffectiveMessageIDs})
		}
		return out
	}
	sortedKeys := func(m map[string]bool) []string {
		ks := make([]string, 0, len(m))
		for k := range m {
			ks = append(ks, k)
		}
		sort.Strings(ks)
		return ks
	}
	states := map[string][]noa.CompressionBlock{
		"empty": {},
		"all-inactive": {
			mkBlk("b1", 1, "m00001", "m00005", false, []string{"h1", "h2"}),
			mkBlk("b2", 2, "m00006", "m00010", false, []string{"h3"}),
		},
		"mix": {
			mkBlk("b1", 1, "m00001", "m00005", true, []string{"h1", "h2"}),
			mkBlk("b2", 2, "m00006", "m00010", false, []string{"h3"}),
			mkBlk("b3", 3, "m00001", "m00020", true, []string{"h4"}),
		},
		"overlap-eff": {
			mkBlk("b1", 1, "m00001", "m00005", true, []string{"h1", "h2", "h3"}),
			mkBlk("b2", 2, "m00003", "m00008", true, []string{"h3", "h4", "h5"}),
		},
		"unsorted-spans": {
			mkBlk("b9", 1, "m00020", "m00030", true, []string{"h1"}),
			mkBlk("b2", 1, "m00001", "m00005", true, []string{"h2"}),
			mkBlk("b5", 2, "m00010", "m00015", true, []string{"h3"}),
		},
		"equal-start": {
			mkBlk("b2", 1, "m00005", "m00008", true, []string{"h1"}),
			mkBlk("b1", 1, "m00005", "m00010", true, []string{"h2"}),
		},
	}
	order := []string{"empty", "all-inactive", "mix", "overlap-eff", "unsorted-spans", "equal-start"}
	for _, name := range order {
		blocks := states[name]
		st := noa.CompressionState{Blocks: blocks}
		rec(t, "ActiveBlocks", []any{st}, toOut(noa.ActiveBlocks(st)))
		rec(t, "CoveredMessageIDs", []any{st}, sortedKeys(noa.CoveredMessageIDs(st)))
		rec(t, "ActiveBlockSpans", []any{st}, noa.ActiveBlockSpans(st))
	}
}
