// Package llmoracle records ground-truth outputs of the pure functions of the
// external github.com/Autumn-27/norma/llm message model for the Rust parity harness.
//
// norma/llm lives in the read-only Go module cache, so record.sh cannot copy an oracle
// into it the way the in-repo packages do. Instead this standalone module imports it and
// prints one JSON object per case: {fn, args, ok}. record.sh sorts the lines so the
// output is byte-reproducible.
package llmoracle

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/Autumn-27/norma/llm"
)

func rec(t *testing.T, fn string, args []any, out any) {
	t.Helper()
	c := map[string]any{"fn": fn, "args": args, "ok": out}
	b, _ := json.Marshal(c)
	fmt.Println(string(b))
}

// mustJSON marshals v for use as an arg / expected value (RawMessage keeps it inline).
func mustJSON(v any) json.RawMessage {
	b, _ := json.Marshal(v)
	return b
}

func TestZZOracle(t *testing.T) {
	tu1 := llm.ContentBlock{Type: llm.BlockToolUse, ID: "t1", Name: "run", Input: json.RawMessage(`{"cmd":"ls"}`)}
	tu2 := llm.ContentBlock{Type: llm.BlockToolUse, ID: "t2", Name: "grep", Input: json.RawMessage(`{"pattern":"x"}`)}
	think := llm.ContentBlock{Type: llm.BlockThinking, Thinking: "reasoning", Signature: "sig"}

	messages := []struct {
		name string
		m    llm.Message
	}{
		{"empty", llm.Message{Role: llm.RoleUser, Content: nil}},
		{"one_text", llm.UserText("hello")},
		{"two_text", llm.Message{Role: llm.RoleUser, Content: []llm.ContentBlock{llm.TextBlock("foo"), llm.TextBlock("bar")}}},
		{"mixed", llm.Message{Role: llm.RoleAssistant, Content: []llm.ContentBlock{llm.TextBlock("before"), think, tu1, llm.TextBlock("after")}}},
		{"only_tooluse", llm.Message{Role: llm.RoleAssistant, Content: []llm.ContentBlock{tu1, tu2}}},
		{"no_tooluse", llm.Message{Role: llm.RoleAssistant, Content: []llm.ContentBlock{llm.TextBlock("a"), think}}},
		{"toolresult", llm.Message{Role: llm.RoleUser, Content: []llm.ContentBlock{llm.ToolResultText("t1", "output", false)}}},
	}
	for _, mc := range messages {
		m := mc.m
		rec(t, "Text", []any{mustJSON(m)}, m.Text())
		// ToolUses returns a nil slice when nothing matches; Go marshals nil as `null`.
		rec(t, "ToolUses", []any{mustJSON(m)}, mustJSON(m.ToolUses()))
	}

	type upair struct {
		name string
		a, b llm.Usage
	}
	pairs := []upair{
		{"zeros_plus", llm.Usage{}, llm.Usage{}},
		{"basic", llm.Usage{InputTokens: 10, OutputTokens: 5}, llm.Usage{InputTokens: 3, OutputTokens: 7}},
		{"caches", llm.Usage{CacheReadTokens: 100, CacheWriteTokens: 20}, llm.Usage{CacheReadTokens: 40, CacheWriteTokens: 80}},
		{"mixed", llm.Usage{InputTokens: 1, OutputTokens: 2, CacheReadTokens: 3, CacheWriteTokens: 4}, llm.Usage{InputTokens: 9, OutputTokens: 8, CacheReadTokens: 7, CacheWriteTokens: 6}},
	}
	for _, p := range pairs {
		ua := p.a
		ua.Add(p.b)
		rec(t, "Add", []any{mustJSON(p.a), mustJSON(p.b)}, ua)

		um := p.a
		um.MergeEcho(p.b)
		rec(t, "MergeEcho", []any{mustJSON(p.a), mustJSON(p.b)}, um)
	}

	// MergeEcho recovery: message_start reports zeros, message_delta carries the real
	// input/cache counts plus the first output increment.
	start := llm.Usage{OutputTokens: 4}
	echo := llm.Usage{InputTokens: 50, OutputTokens: 6, CacheReadTokens: 30, CacheWriteTokens: 10}
	u := start
	u.MergeEcho(echo)
	rec(t, "MergeEcho", []any{mustJSON(start), mustJSON(echo)}, u)

	// MergeEcho where the echo's input is lower: the max keeps the earlier, higher value.
	start2 := llm.Usage{InputTokens: 100}
	lower := llm.Usage{InputTokens: 20, OutputTokens: 3}
	u2 := start2
	u2.MergeEcho(lower)
	rec(t, "MergeEcho", []any{mustJSON(start2), mustJSON(lower)}, u2)
}