// Package noaoracle records ground-truth outputs of the pure, value-typed helpers of
// the external github.com/Autumn-27/norma/noa package for the Rust parity harness.
//
// norma/noa lives in the read-only Go module cache, so record.sh cannot copy an oracle
// into it. Instead this standalone module imports it and prints one JSON object per
// case: {fn, args, ok}. record.sh sorts the lines so the output is byte-reproducible.
// Only exported functions are reachable here.
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

func TestZZOracle(t *testing.T) {
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