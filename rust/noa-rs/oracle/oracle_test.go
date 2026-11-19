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