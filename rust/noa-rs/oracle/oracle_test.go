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
	"strings"
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

// stateBlk is the observable subset of CompressionBlock for DecideTier's state.
type stateBlk struct {
	BlockID string
	Tier    int
	Active  bool
}

// decOut is the observable TierDecision.
type decOut struct {
	TargetTier        int
	OutputTier        int
	ConsumedBlockIDs  []string
	SurvivingBlockIDs []string
}

func TestZZOracle9m(t *testing.T) {
	mkState := func(blocks []stateBlk) noa.CompressionState {
		var out []noa.CompressionBlock
		for _, b := range blocks {
			out = append(out, noa.CompressionBlock{BlockID: b.BlockID, Tier: noa.Tier(b.Tier), Active: b.Active})
		}
		return noa.CompressionState{Blocks: out}
	}
	toDec := func(d noa.TierDecision) decOut {
		return decOut{int(d.TargetTier), int(d.OutputTier), d.ConsumedBlockIDs, d.SurvivingBlockIDs}
	}

	// ---- DecideTier ----
	type dt struct {
		kind    noa.BoundaryKind
		nested  []string
		blocks  []stateBlk
		maxTier int
	}
	dtCases := []dt{
		// pure message range -> tier1/tier1
		{noa.BoundaryMessage, nil, nil, 3},
		{noa.BoundaryMessage, []string{"b1"}, []stateBlk{{"b1", 1, true}}, 3},
		// block range, single tier-1 active -> target 1, output 2, consumed b1
		{noa.BoundaryBlock, []string{"b1"}, []stateBlk{{"b1", 1, true}}, 3},
		// block range, single tier-2 active -> target 2, output 3 (maxTier 3), consumed b2
		{noa.BoundaryBlock, []string{"b2"}, []stateBlk{{"b2", 2, true}}, 3},
		// block range, tier-2 with maxTier 2 -> output capped 2
		{noa.BoundaryBlock, []string{"b2"}, []stateBlk{{"b2", 2, true}}, 2},
		// mixed tiers 1+2 -> target 1 (lowest), consume tier1, survive tier2
		{noa.BoundaryBlock, []string{"b1", "b2"}, []stateBlk{{"b1", 1, true}, {"b2", 2, true}}, 3},
		// mixed tiers 2+3 -> target 2, consume tier2, survive tier3
		{noa.BoundaryBlock, []string{"b2", "b3"}, []stateBlk{{"b2", 2, true}, {"b3", 3, true}}, 3},
		// inactive block ignored -> lowest stays 0 -> target 1
		{noa.BoundaryBlock, []string{"b2"}, []stateBlk{{"b2", 2, false}}, 3},
		// missing block ignored
		{noa.BoundaryBlock, []string{"bX"}, []stateBlk{{"b1", 1, true}}, 3},
		// maxTier < 2 -> clamped to 3
		{noa.BoundaryBlock, []string{"b1"}, []stateBlk{{"b1", 1, true}}, 0},
		{noa.BoundaryBlock, []string{"b2"}, []stateBlk{{"b2", 2, true}}, 1},
		// tier-3 block with maxTier 3 -> target 3, output 3 (min(3,4))
		{noa.BoundaryBlock, []string{"b3"}, []stateBlk{{"b3", 3, true}}, 3},
		// multiple same-tier -> all consumed
		{noa.BoundaryBlock, []string{"b1", "b1b"}, []stateBlk{{"b1", 1, true}, {"b1b", 1, true}}, 3},
	}
	for _, c := range dtCases {
		st := mkState(c.blocks)
		cfg := noa.Config{}
		cfg.Tiers.MaxTier = noa.Tier(c.maxTier)
		r := noa.ResolvedRange{Kind: c.kind, NestedBlockIDs: c.nested}
		arg := map[string]any{
			"Kind":           c.kind,
			"NestedBlockIDs": c.nested,
			"MaxTier":        c.maxTier,
		}
		rec(t, "DecideTier", []any{arg, c.blocks}, toDec(noa.DecideTier(r, st, cfg)))
	}

	// ---- IsTerminalRewrite ----
	type it struct {
		targetTier int
		directCnt  int
		maxTier    int
	}
	itCases := []it{
		{1, 0, 3},  // not terminal
		{3, 0, 3},  // terminal, no new messages -> true
		{3, 2, 3},  // terminal but has direct messages -> false
		{2, 0, 2},  // terminal at maxTier2 -> true
		{2, 1, 2},  // maxTier2 with direct -> false
		{2, 0, 3},  // target 2 but maxTier 3 -> not terminal
		{1, 0, 1},  // maxTier<2 -> clamped 3, target1 not terminal
		{3, 0, 1},  // maxTier<2 -> clamped 3, target3 terminal -> true
	}
	for _, c := range itCases {
		cfg := noa.Config{}
		cfg.Tiers.MaxTier = noa.Tier(c.maxTier)
		d := noa.TierDecision{TargetTier: noa.Tier(c.targetTier), OutputTier: noa.Tier(c.targetTier)}
		arg := map[string]any{
			"TargetTier": c.targetTier,
			"DirectCount": c.directCnt,
			"MaxTier":     c.maxTier,
		}
		rec(t, "IsTerminalRewrite", []any{arg}, noa.IsTerminalRewrite(d, c.directCnt, cfg))
	}
}

func TestZZOracle9n(t *testing.T) {
	msg := func(id, role, ct, text string) noa.CoreMessage {
		return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text}
	}
	// build a MessageRefMap: byRaw hostID->ref, byRef ref->hostID (filled inverse for well-formed).
	mkRefs := func(pairs [][2]string, blocked ...string) noa.MessageRefMap {
		byRaw := map[string]string{}
		byRef := map[string]string{}
		for _, p := range pairs {
			byRaw[p[0]] = p[1]
			byRef[p[1]] = p[0]
		}
		for _, b := range blocked {
			byRaw[b] = noa.BlockedRef
		}
		return noa.MessageRefMap{ByRaw: byRaw, ByRef: byRef}
	}
	type sc struct {
		name    string
		msgs    []noa.CoreMessage
		refs    noa.MessageRefMap
		blocks  []noa.CompressionBlock
		pMsg    int
		pTok    int
	}
	activeBlk := func(id string, eff []string) noa.CompressionBlock {
		return noa.CompressionBlock{BlockID: id, Tier: 1, Active: true, EffectiveMessageIDs: eff}
	}
	cases := []sc{
		{
			name: "no-preserve",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "hello world"),
				msg("a2", "assistant", "text", "hi there"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}}),
		},
		{
			name: "preserve-msgs-1",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "one"),
				msg("a2", "assistant", "text", "two"),
				msg("a3", "user", "text", "three"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}, {"a3", "m00003"}}),
			pMsg: 1,
		},
		{
			name: "preserve-toks",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "aaaa"),         // 1 token
				msg("a2", "assistant", "text", "bbbbbbbbbbbbbbbbbbbb"), // ~4 tokens
				msg("a3", "user", "text", "cc"),           // 1 token
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}, {"a3", "m00003"}}),
			pTok: 2, // acc from tail: a3(1) -> acc1<2 add; a2(4) -> acc5>=2 stop => a3,a2
		},
		{
			name: "covered-skip",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "one"),
				msg("a2", "assistant", "text", "two"),
				msg("a3", "user", "text", "three"),
			},
			refs:   mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}, {"a3", "m00003"}}),
			blocks: []noa.CompressionBlock{activeBlk("b1", []string{"a2"})},
			pMsg:   1, // a3 not covered -> protected; a2 covered -> skip
		},
		{
			name: "summary-skip",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "one"),
				msg(noa.SummaryIDPrefix+"b1", "user", "text", noa.SummaryHeader+" body"),
				msg("a2", "user", "text", "two"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00003"}}),
			pMsg: 1, // summary skipped from visible; only a1,a2 visible -> a2 protected
		},
		{
			name: "blocked-skip",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "one"),
				msg("a2", "user", "text", "two"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}}, "a2"),
			pMsg: 1, // a2 blocked -> skip; only a1 visible -> a1 protected
		},
		{
			name: "never-preserve-tool-skip",
			msgs: []noa.CoreMessage{
				func() noa.CoreMessage { m := msg("a1", "tool", "tool-result", "out"); m.ToolName = "Read"; return m }(),
				msg("a2", "user", "text", "two"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}}),
			pMsg: 1, // a1 Read skipped from visible; a2 protected
		},
		{
			name: "empty-id-skip",
			msgs: []noa.CoreMessage{
				msg("", "user", "text", "ghost"),
				msg("a1", "user", "text", "real"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}}),
			pMsg: 1, // empty id skipped
		},
		{
			name: "both-preserve",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "one"),
				msg("a2", "assistant", "text", "two"),
				msg("a3", "user", "text", "three"),
				msg("a4", "assistant", "text", "four"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}, {"a3", "m00003"}, {"a4", "m00004"}}),
			pMsg: 2, // last 2: a3,a4
			pTok: 1, // tail accum: a4(1)>=1 -> add a4, stop
		},
	}
	for _, c := range cases {
		st := noa.CompressionState{Blocks: c.blocks, MessageRefs: c.refs}
		cfg := noa.Config{PreserveRecentMessages: c.pMsg, PreserveRecentTokens: c.pTok}
		got := noa.ComputeProtectedRefs(c.msgs, st, cfg)
		keys := make([]string, 0, len(got))
		for k := range got {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		arg := map[string]any{
			"PreserveRecentMessages": c.pMsg,
			"PreserveRecentTokens":   c.pTok,
		}
		rec(t, "ComputeProtectedRefs", []any{c.msgs, c.refs, c.blocks, arg}, keys)
	}
}

func TestZZOracle9o(t *testing.T) {
	cm := func(id, role, ct, text, tool, callID string) noa.CoreMessage {
		return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text, ToolName: tool, ToolCallID: callID}
	}
	mkRefs := func(byRaw map[string]string) noa.MessageRefMap {
		byRef := map[string]string{}
		for raw, ref := range byRaw {
			if ref != noa.BlockedRef {
				byRef[ref] = raw
			}
		}
		return noa.MessageRefMap{ByRaw: byRaw, ByRef: byRef}
	}
	activeBlk := func(id string, eff []string) noa.CompressionBlock {
		return noa.CompressionBlock{BlockID: id, Tier: 1, Active: true, EffectiveMessageIDs: eff}
	}
	type sc struct {
		name       string
		msgs       []noa.CoreMessage
		byRaw      map[string]string
		blocks     []noa.CompressionBlock
		pTools     []string
		protected  map[string]bool
	}
	cases := []sc{
		{
			// pure compressible run: 2 text + 1 tool-call, grouped into 1 range
			name: "simple-run",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "hello world", "", ""),
				cm("a2", "assistant", "tool-call", "call tool", "Bash", ""),
				cm("a3", "tool", "tool-result", "output here", "", ""),
			},
			byRaw: map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
		},
		{
			// a covered message interrupts the run -> gapBefore on the next
			name: "gap-covered",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "first", "", ""),
				cm("a2", "assistant", "text", "covered one", "", ""),
				cm("a3", "user", "text", "third", "", ""),
			},
			byRaw:  map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			blocks: []noa.CompressionBlock{activeBlk("b1", []string{"a2"})},
		},
		{
			// protected tool message forms a ProtectedRange, breaking the run
			name: "protected-write",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "do it", "", ""),
				cm("a2", "assistant", "tool-call", "write call", "Write", "call1"),
				cm("a3", "user", "text", "after", "", ""),
			},
			byRaw:  map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			pTools: []string{"Write"},
		},
		{
			// tool-result paired with a protected tool-call is also protected
			name: "pairing-result",
			msgs: []noa.CoreMessage{
				cm("a1", "assistant", "tool-call", "write call", "Write", "call1"),
				cm("a2", "tool", "tool-result", "write result", "", "call1"),
				cm("a3", "user", "text", "tail text", "", ""),
			},
			byRaw:  map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			pTools: []string{"Write"},
		},
		{
			// protectedRefs gap
			name: "protected-ref-gap",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "aa", "", ""),
				cm("a2", "assistant", "text", "bb", "", ""),
				cm("a3", "user", "text", "cc", "", ""),
			},
			byRaw:     map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			protected: map[string]bool{"m00002": true},
		},
		{
			// consecutive protected tools accumulate into one ProtectedRange with tool list
			name: "multi-protected",
			msgs: []noa.CoreMessage{
				cm("a1", "assistant", "tool-call", "w", "Write", "c1"),
				cm("a2", "assistant", "tool-call", "e", "Edit", "c2"),
				cm("a3", "user", "text", "tail", "", ""),
			},
			byRaw:  map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			pTools: []string{"Write", "Edit"},
		},
		{
			// user message breaks run at count>=3
			name: "user-break",
			msgs: []noa.CoreMessage{
				cm("a1", "assistant", "text", "1", "", ""),
				cm("a2", "assistant", "text", "2", "", ""),
				cm("a3", "assistant", "text", "3", "", ""),
				cm("a4", "user", "text", "interrupt", "", ""),
				cm("a5", "assistant", "text", "5", "", ""),
			},
			byRaw: map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003", "a4": "m00004", "a5": "m00005"},
		},
		{
			// unaddressable messages (empty id / no ref / blocked) interrupt nothing
			name: "skip-unaddressable",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "keep1", "", ""),
				cm("", "user", "text", "ghost", "", ""),
				cm("a2", "user", "text", "blocked", "", ""),
				cm("a3", "user", "text", "keep2", "", ""),
			},
			byRaw: map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			blocks: nil,
			// a2 has no ref assigned -> unaddressable; a3 keep. Wait a2 needs blocked:
		},
		{
			// blocked ref specifically
			name: "blocked-ref",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "keep1", "", ""),
				cm("a2", "user", "text", "is blocked", "", ""),
				cm("a3", "user", "text", "keep2", "", ""),
			},
			byRaw: map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
		},
		{
			// summary message text (header prefix) -> gap
			name: "summary-gap",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "before", "", ""),
				cm("a2", "user", "text", noa.SummaryHeader+" body", "", ""),
				cm("a3", "user", "text", "after", "", ""),
			},
			byRaw: map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
		},
		{
			// all unaddressable -> empty ranges (nil)
			name: "all-empty",
			msgs: []noa.CoreMessage{
				cm("", "user", "text", "ghost", "", ""),
			},
			byRaw: map[string]string{},
		},
		{
			// BlockedRef message and a message with NO ref both interrupt nothing
			name: "blocked-and-noref",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "keep1", "", ""),
				cm("a2", "user", "text", "is blocked", "", ""),
				cm("a3", "user", "text", "no ref here", "", ""),
				cm("a4", "user", "text", "keep2", "", ""),
			},
			// a2 -> BlockedRef (skip), a3 -> not in map at all (hasRef=false, skip)
			byRaw: map[string]string{"a1": "m00001", "a2": noa.BlockedRef, "a4": "m00004"},
		},
	}
	for _, c := range cases {
		st := noa.CompressionState{Blocks: c.blocks, MessageRefs: mkRefs(c.byRaw)}
		cfg := noa.Config{ProtectedTools: c.pTools}
		if c.protected == nil {
			c.protected = map[string]bool{}
		}
		ranges, protected := noa.BuildCompressibleRanges(c.msgs, st, cfg, c.protected, nil)
		cfgArg := map[string]any{"ProtectedTools": c.pTools}
		protectedArg := make([]string, 0, len(c.protected))
		for k := range c.protected {
			protectedArg = append(protectedArg, k)
		}
		sort.Strings(protectedArg)
		rec(t, "BuildCompressibleRanges",
			[]any{c.msgs, c.byRaw, c.blocks, cfgArg, protectedArg},
			[]any{ranges, protected})
	}
}

func TestZZOracle9p(t *testing.T) {
	cm := func(role, ct, text string) noa.CoreMessage {
		return noa.CoreMessage{Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text}
	}
	type sc struct {
		name string
		msgs []noa.CoreMessage
	}
	cases := []sc{
		{name: "empty", msgs: nil},
		{
			name: "all-text",
			msgs: []noa.CoreMessage{
				cm("user", "text", "hello world"),
				cm("assistant", "text", "general kenobi"),
			},
		},
		{
			name: "tool-call-and-result",
			msgs: []noa.CoreMessage{
				cm("assistant", "tool-call", "invoke bash"),
				cm("tool", "tool-result", "file contents here"),
			},
		},
		{
			name: "code-fence",
			msgs: []noa.CoreMessage{
				cm("assistant", "text", "here is code:\n```go\nfmt.Println()\n```"),
			},
		},
		{
			name: "summary-header",
			msgs: []noa.CoreMessage{
				cm("user", "text", noa.SummaryHeader+" earlier work"),
			},
		},
		{
			// summary header takes precedence over tool content-type
			name: "summary-over-tool",
			msgs: []noa.CoreMessage{
				cm("user", "tool-result", noa.SummaryHeader+" x"),
			},
		},
		{
			// tool content-type takes precedence over code fence in text
			name: "tool-over-code",
			msgs: []noa.CoreMessage{
				cm("tool", "tool-result", "output ```code``` here"),
			},
		},
		{
			name: "mixed-all",
			msgs: []noa.CoreMessage{
				cm("user", "text", "plain text"),
				cm("assistant", "text", "```py\nx=1\n```"),
				cm("assistant", "tool-call", "call"),
				cm("tool", "tool-result", "result"),
				cm("user", "text", noa.SummaryHeader+" done"),
			},
		},
		{
			// empty text contributes 0 tokens but still classified as text
			name: "empty-text",
			msgs: []noa.CoreMessage{
				cm("user", "text", ""),
			},
		},
	}
	for _, c := range cases {
		got := noa.ComputeContextBreakdown(c.msgs, nil)
		rec(t, "ComputeContextBreakdown", []any{c.msgs}, got)
	}
}

func TestZZOracle9q(t *testing.T) {
	cm := func(id, role, ct, text, tool string) noa.CoreMessage {
		return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text, ToolName: tool}
	}
	mkRefs := func(byRaw map[string]string) noa.MessageRefMap {
		byRef := map[string]string{}
		for raw, ref := range byRaw {
			if ref != noa.BlockedRef {
				byRef[ref] = raw
			}
		}
		return noa.MessageRefMap{ByRaw: byRaw, ByRef: byRef}
	}
	// big text to clear MinCompressRange and ViableRangeMinTokens
	big := func(n int) string {
		b := make([]byte, n)
		for i := range b {
			b[i] = 'a'
		}
		return string(b)
	}
	type sc struct {
		name       string
		msgs       []noa.CoreMessage
		byRaw      map[string]string
		blocks     []noa.CompressionBlock
		pTools     []string
		minRange   int
		pMsg, pTok int
	}
	cases := []sc{
		{
			name:  "empty",
			byRaw: map[string]string{},
		},
		{
			// small text -> merged below minRange -> viable empty -> nothing to compress
			name: "too-small",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", "hi", ""),
			},
			byRaw:    map[string]string{"a1": "m00001"},
			minRange: 1000,
		},
		{
			// large single span clears minRange and viable tokens
			name: "viable-single",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", big(2000), ""),
			},
			byRaw:    map[string]string{"a1": "m00001"},
			minRange: 1000,
		},
		{
			// two spans merged across a protected tool
			name: "protected-between",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", big(1500), ""),
				cm("a2", "assistant", "tool-call", "write", "Write"),
				cm("a3", "user", "text", big(1500), ""),
			},
			byRaw:    map[string]string{"a1": "m00001", "a2": "m00002", "a3": "m00003"},
			pTools:   []string{"Write"},
			minRange: 1000,
		},
		{
			// preserve-recent removes the tail from compressibility
			name: "preserve-tail",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", big(1500), ""),
				cm("a2", "assistant", "text", big(1500), ""),
			},
			byRaw:    map[string]string{"a1": "m00001", "a2": "m00002"},
			minRange: 1000,
			pMsg:     1, // a2 preserved -> only a1 compressible
		},
		{
			// minRange 0 -> merge passthrough (no merge), single big span viable
			name: "minrange-zero",
			msgs: []noa.CoreMessage{
				cm("a1", "user", "text", big(2000), ""),
			},
			byRaw:    map[string]string{"a1": "m00001"},
			minRange: 0,
		},
	}
	for _, c := range cases {
		st := noa.CompressionState{Blocks: c.blocks, MessageRefs: mkRefs(c.byRaw)}
		cfg := noa.Config{}
		cfg.ProtectedTools = c.pTools
		cfg.Compress.MinCompressRange = c.minRange
		cfg.PreserveRecentMessages = c.pMsg
		cfg.PreserveRecentTokens = c.pTok
		rec0 := noa.BuildRecommendation(c.msgs, st, cfg, nil)
		rec(t, "BuildRecommendation",
			[]any{c.msgs, c.byRaw, c.blocks, map[string]any{
				"ProtectedTools":         c.pTools,
				"MinCompressRange":       c.minRange,
				"PreserveRecentMessages": c.pMsg,
				"PreserveRecentTokens":   c.pTok,
			}},
			rec0)
	}
}

// TestZZOracle9n2 adds focused regression cases for ComputeProtectedRefs's THIRD rule:
// when PreserveRecentMessages > 0, the most-recent *user* message is always protected,
// even when it already fell outside the recent-messages window. These cases distinguish
// that rule from the recent-window alone (a naive port that omits it fails them).
func TestZZOracle9n2(t *testing.T) {
	msg := func(id, role, ct, text string) noa.CoreMessage {
		return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text}
	}
	mkRefs := func(pairs [][2]string) noa.MessageRefMap {
		byRaw := map[string]string{}
		byRef := map[string]string{}
		for _, p := range pairs {
			byRaw[p[0]] = p[1]
			byRef[p[1]] = p[0]
		}
		return noa.MessageRefMap{ByRaw: byRaw, ByRef: byRef}
	}
	type sc struct {
		name    string
		msgs    []noa.CoreMessage
		refs    noa.MessageRefMap
		pMsg    int
		pTok    int
	}
	cases := []sc{
		{
			// recent window = {a2 asst}; 3rd rule adds a1 (most-recent user).
			name: "recent-window-excludes-user",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "the live instruction"),
				msg("a2", "assistant", "text", "the answer"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}}),
			pMsg: 1,
		},
		{
			// recent window (2) = {a2 asst, a3 asst}; 3rd rule adds a1 (most-recent user).
			name: "two-recent-nonuser",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "instruction"),
				msg("a2", "assistant", "text", "step one"),
				msg("a3", "assistant", "text", "step two"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}, {"a3", "m00003"}}),
			pMsg: 2,
		},
		{
			// most-recent user already inside the window -> no change (sanity).
			name: "user-inside-window",
			msgs: []noa.CoreMessage{
				msg("a1", "assistant", "text", "old answer"),
				msg("a2", "user", "text", "new instruction"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}}),
			pMsg: 1,
		},
		{
			// pMsg=0 -> 3rd rule disabled entirely.
			name: "pmsg-zero-no-rule",
			msgs: []noa.CoreMessage{
				msg("a1", "user", "text", "instruction"),
				msg("a2", "assistant", "text", "answer"),
			},
			refs: mkRefs([][2]string{{"a1", "m00001"}, {"a2", "m00002"}}),
			pMsg: 0,
		},
	}
	for _, c := range cases {
		st := noa.CompressionState{MessageRefs: c.refs}
		cfg := noa.Config{PreserveRecentMessages: c.pMsg, PreserveRecentTokens: c.pTok}
		got := noa.ComputeProtectedRefs(c.msgs, st, cfg)
		keys := make([]string, 0, len(got))
		for k := range got {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		arg := map[string]any{
			"PreserveRecentMessages": c.pMsg,
			"PreserveRecentTokens":   c.pTok,
		}
		rec(t, "ComputeProtectedRefs", []any{c.msgs, c.refs, nil, arg}, keys)
	}
}

// TestZZOracle9r records DecideNudge — the nudge-decision capstone. It composes the
// unexported helpers (resolveAdaptiveGrowth, growthFloorOf, minPressureBenefit,
// computeTierPending, tierCadenceReady, pendingSummary, decidePressure, decideGrowth),
// all reached transitively through the exported DecideNudge. Cases cover both bands
// (pressure / growth), tier-1/2/3 selection, cadence gating, tiers-disabled, the
// first-sight path, and the passthrough fields (ranges, block spans, truncation).
func TestZZOracle9r(t *testing.T) {
	big := func(n int) string { return strings.Repeat("a", n) }
	cm := func(id, role, ct, text string) noa.CoreMessage {
		return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text}
	}
	mkBlocks := func(tier noa.Tier, n, summaryLen int) []noa.CompressionBlock {
		out := make([]noa.CompressionBlock, 0, n)
		for i := 0; i < n; i++ {
			ref := fmt.Sprintf("m%05d", i)
			out = append(out, noa.CompressionBlock{
				BlockID:             fmt.Sprintf("b%d", i),
				Tier:                tier,
				Topic:               "topic",
				Summary:             big(summaryLen),
				EffectiveMessageIDs: []string{fmt.Sprintf("msg%d", i)},
				Active:              true,
				StartRef:            ref,
				EndRef:              ref,
			})
		}
		return out
	}
	cfgMap := func(c noa.Config) map[string]any {
		var mp any
		if c.Nudge.MinPressureBenefitTokens != nil {
			mp = *c.Nudge.MinPressureBenefitTokens
		}
		return map[string]any{
			"ModelContextLimit": c.ModelContextLimit,
			"Tiers": map[string]any{
				"Enabled": c.Tiers.Enabled, "Tier2Trigger": c.Tiers.Tier2Trigger, "Tier3Trigger": c.Tiers.Tier3Trigger,
			},
			"Nudge": map[string]any{
				"MaxContextLimitPct": c.Nudge.MaxContextLimitPct, "MinContextLimitPct": c.Nudge.MinContextLimitPct,
				"EmergencyThresholdPct": c.Nudge.EmergencyThresholdPct, "GrowthRatio": c.Nudge.GrowthRatio,
				"GrowthFloor": c.Nudge.GrowthFloor, "GrowthCap": c.Nudge.GrowthCap,
				"MinGrowthFloor": c.Nudge.MinGrowthFloor, "MinGrowthRatio": c.Nudge.MinGrowthRatio,
				"Tier2GrowthMultiplier": c.Nudge.Tier2GrowthMultiplier, "MinPressureBenefitTokens": mp,
			},
			"Compress": map[string]any{"MinCompressRange": c.Compress.MinCompressRange},
		}
	}
	run := func(in noa.DecideNudgeInput) {
		arg := map[string]any{
			"Messages": in.Messages,
			"Blocks":   in.State.Blocks,
			"Nudge": map[string]any{
				"LastPerMessageNudgeTokens": in.State.Nudge.LastPerMessageNudgeTokens,
				"LastNudgeShownTokens":      in.State.Nudge.LastNudgeShownTokens,
				"LastShownByTier":           in.State.Nudge.LastShownByTier,
			},
			"Config":     cfgMap(in.Config),
			"TokenCount": in.TokenCount,
			"Recommendation": map[string]any{
				"CompressibleRanges": in.Recommendation.CompressibleRanges,
				"ProtectedRanges":    in.Recommendation.ProtectedRanges,
			},
			"TruncatedCount": in.TruncatedCount,
		}
		rec(t, "DecideNudge", []any{arg}, noa.DecideNudge(in))
	}

	rng := func(refs string, tokens, chars, count int) noa.RangeInfo {
		return noa.RangeInfo{StartRef: refs, EndRef: refs, Count: count, Tokens: tokens, Chars: chars, TextPct: 100, UserMsgs: count}
	}

	// A: limit<=0 -> usage 0, below cadence, no injection.
	run(noa.DecideNudgeInput{Config: noa.DefaultConfig(0), TokenCount: 50000})

	// B: pressure but nothing worth reclaiming (no compressible, no blocks).
	run(noa.DecideNudgeInput{
		Config:     noa.DefaultConfig(200000),
		TokenCount: 160000,
		Messages:   []noa.CoreMessage{cm("a1", "user", "text", big(100))},
	})

	// C: pressure, tier-1 reclaim clears the benefit floor -> inject T1.
	run(noa.DecideNudgeInput{
		Config:         noa.DefaultConfig(200000),
		TokenCount:     160000,
		Messages:       []noa.CoreMessage{cm("a1", "user", "text", big(100))},
		Recommendation: noa.Recommendation{CompressibleRanges: []noa.RangeInfo{rng("m00001", 12000, 9000, 3)}},
	})

	// D: emergency band (0.975) still resolves to tier 1; exercises %.0f rounding of 97.5.
	run(noa.DecideNudgeInput{
		Config:         noa.DefaultConfig(200000),
		TokenCount:     195000,
		Messages:       []noa.CoreMessage{cm("a1", "user", "text", big(80))},
		Recommendation: noa.Recommendation{CompressibleRanges: []noa.RangeInfo{rng("m00001", 20000, 15000, 4)}},
	})

	// E: pressure, tier-2 reclaims the most (5 active tier-1 blocks) -> inject T2 + targets.
	run(noa.DecideNudgeInput{
		Config:     noa.DefaultConfig(200000),
		TokenCount: 170000,
		State:      noa.CompressionState{Blocks: mkBlocks(1, 5, 5000)},
	})

	// F: same as E but tiers disabled -> only tier 1 is a candidate, nothing clears -> no inject.
	cfgF := noa.DefaultConfig(200000)
	cfgF.Tiers.Enabled = false
	run(noa.DecideNudgeInput{
		Config:     cfgF,
		TokenCount: 170000,
		State:      noa.CompressionState{Blocks: mkBlocks(1, 5, 5000)},
	})

	// G: growth band, not enough growth since the last nudge -> below cadence.
	stG := noa.CompressionState{}
	stG.Nudge.LastNudgeShownTokens = 50000
	run(noa.DecideNudgeInput{Config: noa.DefaultConfig(200000), TokenCount: 60000, State: stG})

	// H: growth band, tier 1 clears the adaptive floor -> inject T1.
	stH := noa.CompressionState{}
	stH.Nudge.LastNudgeShownTokens = 10000
	run(noa.DecideNudgeInput{
		Config:         noa.DefaultConfig(200000),
		TokenCount:     100000,
		State:          stH,
		Recommendation: noa.Recommendation{CompressibleRanges: []noa.RangeInfo{rng("m00001", 60000, 50000, 5)}},
	})

	// I: growth, tier 2 via count trigger (5 blocks, usage above the floor) -> inject T2.
	stI := noa.CompressionState{Blocks: mkBlocks(1, 5, 400)}
	stI.Nudge.LastNudgeShownTokens = 10000
	run(noa.DecideNudgeInput{Config: noa.DefaultConfig(200000), TokenCount: 100000, State: stI})

	// J: same as I but tier 2 is cadence-blocked -> suppressed, reason carries the gate.
	stJ := noa.CompressionState{Blocks: mkBlocks(1, 5, 400)}
	stJ.Nudge.LastNudgeShownTokens = 10000
	stJ.Nudge.LastShownByTier = map[noa.Tier]int{2: 95000}
	run(noa.DecideNudgeInput{Config: noa.DefaultConfig(200000), TokenCount: 100000, State: stJ})

	// K: growth, tier 3 via count trigger (10 tier-2 blocks) -> inject T3 + targets.
	stK := noa.CompressionState{Blocks: mkBlocks(2, 10, 400)}
	stK.Nudge.LastNudgeShownTokens = 10000
	run(noa.DecideNudgeInput{Config: noa.DefaultConfig(200000), TokenCount: 100000, State: stK})

	// L: growth ready but no tier qualifies (3 tier-1 blocks, below both triggers).
	stL := noa.CompressionState{Blocks: mkBlocks(1, 3, 400)}
	stL.Nudge.LastNudgeShownTokens = 10000
	run(noa.DecideNudgeInput{Config: noa.DefaultConfig(200000), TokenCount: 100000, State: stL})

	// M: first sight — a backlog with no baseline still injects despite zero growth.
	run(noa.DecideNudgeInput{
		Config:         noa.DefaultConfig(200000),
		TokenCount:     100000,
		Recommendation: noa.Recommendation{CompressibleRanges: []noa.RangeInfo{rng("m00001", 60000, 50000, 5)}},
	})

	// N: protected ranges pass through on an injected tier-1 nudge.
	run(noa.DecideNudgeInput{
		Config:     noa.DefaultConfig(200000),
		TokenCount: 160000,
		Recommendation: noa.Recommendation{
			CompressibleRanges: []noa.RangeInfo{rng("m00001", 12000, 9000, 3)},
			ProtectedRanges:    []noa.ProtectedRange{{StartRef: "m00004", EndRef: "m00004", Count: 1, Tokens: 30, Tools: []string{"Write"}}},
		},
	})

	// O: truncation count passes through on an injected nudge.
	run(noa.DecideNudgeInput{
		Config:         noa.DefaultConfig(200000),
		TokenCount:     160000,
		Recommendation: noa.Recommendation{CompressibleRanges: []noa.RangeInfo{rng("m00001", 12000, 9000, 3)}},
		TruncatedCount: 3,
	})
}

// TestZZOracle9s records three small pure helpers: CountMessageTokens (token count of
// one message via the resolved callback), NudgeGrowthFloor (adaptive growth floor for a
// config), and TruncateLargeToolOutputs (the mechanical largest-first tool-result cut,
// with truncateBody reached transitively).
func TestZZOracle9s(t *testing.T) {
	big := func(ch string, n int) string { return strings.Repeat(ch, n) }

	// ---- CountMessageTokens (nil count -> DefaultCountTokens) ----
	cm := func(id, role, ct, text string) noa.CoreMessage {
		return noa.CoreMessage{ID: id, Role: noa.Role(role), ContentType: noa.ContentType(ct), Text: text}
	}
	for _, m := range []noa.CoreMessage{
		cm("a1", "user", "text", ""),
		cm("a2", "user", "text", "hi"),
		cm("a3", "assistant", "tool-call", big("x", 400)),
		cm("a4", "tool", "tool-result", big("中", 250)), // CJK: 250 tokens
		cm("a5", "user", "text", "mixed 中文 text"),
	} {
		rec(t, "CountMessageTokens", []any{m}, noa.CountMessageTokens(m, nil))
	}

	// ---- NudgeGrowthFloor (exercises resolveAdaptiveGrowth + growthFloorOf) ----
	growthRec := func(cfg noa.Config) {
		arg := map[string]any{
			"ModelContextLimit": cfg.ModelContextLimit,
			"Nudge": map[string]any{
				"GrowthRatio": cfg.Nudge.GrowthRatio, "GrowthFloor": cfg.Nudge.GrowthFloor,
				"GrowthCap": cfg.Nudge.GrowthCap, "MinGrowthFloor": cfg.Nudge.MinGrowthFloor,
				"MinGrowthRatio": cfg.Nudge.MinGrowthRatio,
			},
		}
		rec(t, "NudgeGrowthFloor", []any{arg}, noa.NudgeGrowthFloor(cfg))
	}
	growthRec(noa.DefaultConfig(200000)) // floor 22500
	growthRec(noa.DefaultConfig(0))      // limit<=0 -> growthFloor path, still 22500

	c1 := noa.DefaultConfig(2000000) // ratio*limit=100000 > cap 50000 -> cap
	growthRec(c1)
	c2 := noa.DefaultConfig(1200000)
	c2.Nudge.GrowthCap = 200000 // ratio*limit=60000 in band -> 60000; floor int(0.45*60000)=27000
	growthRec(c2)
	c3 := noa.DefaultConfig(1999980)
	c3.Nudge.GrowthFloor = 0
	c3.Nudge.GrowthCap = 200000
	c3.Nudge.MinGrowthFloor = 0
	c3.Nudge.MinGrowthRatio = 0.333 // adaptive 99999; int(0.333*99999)=33299 (truncation)
	growthRec(c3)
	c4 := noa.DefaultConfig(200000)
	c4.Nudge.GrowthFloor = 0
	c4.Nudge.GrowthCap = 200000
	// adaptive 10000; min_growth_floor 20000 dominates int(0.45*10000)=4500
	growthRec(c4)
	c5 := noa.DefaultConfig(200000)
	c5.Nudge.MinGrowthFloor = 0
	c5.Nudge.MinGrowthRatio = 0.45 // adaptive 50000; int(22500.0)=22500
	growthRec(c5)

	// ---- TruncateLargeToolOutputs ----
	truncRec := func(msgs []noa.CoreMessage, tokenCount int, cfg noa.Config) {
		arg := map[string]any{
			"Msgs":       msgs,
			"TokenCount": tokenCount,
			"Config": map[string]any{
				"ModelContextLimit":      cfg.ModelContextLimit,
				"Truncate":               map[string]any{"Threshold": cfg.Truncate.Threshold},
				"PreserveRecentMessages": cfg.PreserveRecentMessages,
			},
		}
		rec(t, "TruncateLargeToolOutputs", []any{arg}, noa.TruncateLargeToolOutputs(msgs, tokenCount, cfg, nil))
	}

	// T1: limit<=0 -> no-op, messages pass through.
	truncRec([]noa.CoreMessage{cm("a1", "tool", "tool-result", big("x", 12000))}, 195000, noa.DefaultConfig(0))
	// T2: tokenCount below threshold -> no-op.
	truncRec([]noa.CoreMessage{cm("a1", "tool", "tool-result", big("x", 12000))}, 100000, noa.DefaultConfig(200000))
	// T3: above threshold but result too small (<1000 tokens) -> no candidates.
	truncRec([]noa.CoreMessage{cm("a1", "tool", "tool-result", big("x", 1000))}, 195000, noa.DefaultConfig(200000))
	// T4: single large result, preserve=0 -> cut (DefaultConfig pres=5 would protect it).
	cfgT4 := noa.DefaultConfig(200000)
	cfgT4.PreserveRecentMessages = 0
	truncRec([]noa.CoreMessage{cm("a1", "tool", "tool-result", big("x", 12000))}, 195000, cfgT4)
	// T5: preserve=0 -> both candidates eligible; largest-first cuts both to reach target.
	cfg5 := noa.DefaultConfig(20000)
	cfg5.PreserveRecentMessages = 0
	truncRec([]noa.CoreMessage{
		cm("b1", "tool", "tool-result", big("b", 6000)),  // 1500 tokens
		cm("a1", "tool", "tool-result", big("a", 12000)), // 3000 tokens
	}, 19000, cfg5)
	// T12: largest-first single cut — cutting only the big result reaches the target.
	cfg12 := noa.DefaultConfig(20000)
	cfg12.PreserveRecentMessages = 0
	truncRec([]noa.CoreMessage{
		cm("s1", "tool", "tool-result", big("s", 6000)),   // 1500 tokens (kept)
		cm("a1", "tool", "tool-result", big("a", 40000)),  // 10000 tokens (cut)
	}, 17000, cfg12)
	// T6: preserve-recent tail is protected — only index 0 (of 2) is a candidate.
	cfg6 := noa.DefaultConfig(200000)
	cfg6.PreserveRecentMessages = 1
	truncRec([]noa.CoreMessage{
		cm("a1", "tool", "tool-result", big("a", 12000)),
		cm("b1", "tool", "tool-result", big("b", 12000)),
	}, 195000, cfg6)
	// T7: an already-truncated result is skipped; a fresh one is cut.
	truncRec([]noa.CoreMessage{
		cm("a1", "tool", "tool-result", noa.TruncationMarker+big("z", 12000)),
		cm("c1", "tool", "tool-result", big("c", 12000)),
	}, 195000, noa.DefaultConfig(200000))
	// T8: result exactly keepPrefix+keepSuffix runes (4000) -> candidate but body guard skips.
	cfgT8 := noa.DefaultConfig(200000)
	cfgT8.PreserveRecentMessages = 0
	truncRec([]noa.CoreMessage{cm("d1", "tool", "tool-result", big("d", 4000))}, 195000, cfgT8)
	// T9: multibyte (CJK) content, preserve=0 -> rune-correct head/tail slicing.
	cfgT9 := noa.DefaultConfig(200000)
	cfgT9.PreserveRecentMessages = 0
	truncRec([]noa.CoreMessage{cm("u1", "tool", "tool-result", big("中", 5000))}, 195000, cfgT9)
	// T10: a large NON-tool-result message is not a candidate.
	truncRec([]noa.CoreMessage{cm("v1", "assistant", "text", big("v", 12000))}, 195000, noa.DefaultConfig(200000))
	// T11: an empty-text tool result is skipped.
	truncRec([]noa.CoreMessage{cm("e1", "tool", "tool-result", "")}, 195000, noa.DefaultConfig(200000))
	// T-nil: nil message slice -> null Messages, no-op.
	truncRec(nil, 195000, noa.DefaultConfig(200000))
}
