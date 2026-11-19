// Package compactionoracle records ground-truth outputs of the pure functions of the
// external github.com/Autumn-27/norma/compaction package for the Rust parity harness.
//
// norma/compaction lives in the read-only Go module cache, so record.sh cannot copy an
// oracle into it the way the in-repo packages do. Instead this standalone module imports
// it and prints one JSON object per case: {fn, args, ok}. record.sh sorts the lines so
// the output is byte-reproducible. Only exported functions are reachable here.
package compactionoracle

import (
	"encoding/json"
	"errors"
	"fmt"
	"testing"

	"github.com/Autumn-27/norma/compaction"
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

func txt(s string) llm.ContentBlock { return llm.TextBlock(s) }
func asst(bs ...llm.ContentBlock) llm.Message {
	return llm.Message{Role: llm.RoleAssistant, Content: bs}
}
func usr(bs ...llm.ContentBlock) llm.Message {
	return llm.Message{Role: llm.RoleUser, Content: bs}
}

func TestZZOracle(t *testing.T) {
	// compaction.EstimateTokens([]llm.Message) int — Σ blockChars / 4.
	// blockChars: text→len(Text), thinking→len(Thinking), tool_use→len(Name)+len(Input),
	// tool_result→Σ blockChars(nested), default→len(Text).
	msgSets := [][]llm.Message{
		nil,    // -> 0
		{},     // -> 0
		{usr(txt(""))},                 // empty text -> 0
		{usr(txt("hello"))},            // 5 -> 1
		{asst(txt("ab"), txt("cde"))},  // 5 -> 1
		{{Role: llm.RoleAssistant, Content: []llm.ContentBlock{{Type: llm.BlockThinking, Thinking: "reasoning"}}}}, // 9 -> 2
		{asst(llm.ContentBlock{Type: llm.BlockToolUse, ID: "t1", Name: "run", Input: json.RawMessage(`{"cmd":"ls"}`)})}, // name3+input12=15
		{usr(llm.ContentBlock{Type: llm.BlockToolResult, ToolUseID: "t1", Content: []llm.ContentBlock{txt("output")}})}, // 6 -> 1
		{usr(llm.ContentBlock{Type: llm.BlockToolResult, ToolUseID: "t1", Content: []llm.ContentBlock{
			txt("nested"), llm.ContentBlock{Type: llm.BlockToolUse, Name: "deep", Input: json.RawMessage(`{}`)},
		}})}, // nested: 6 + (deep4+input2=6) = 12 -> 3
		{usr(txt("hello world")), asst(txt("foo bar baz"))}, // 11+11 = 22 -> 5
		{usr(llm.ContentBlock{Type: "image", Text: "fallback"})}, // default branch -> len(Text)=8 -> 2
	}
	for _, ms := range msgSets {
		rec(t, "EstimateTokens", []any{mustJSON(ms)}, compaction.EstimateTokens(ms))
	}

	// compaction.IsOverflow(error) bool — nil -> false; case-insensitive substring match.
	// The harness passes the error string (null for nil); Rust models nil as None.
	errStrings := []string{
		"request too long",
		"prompt is TOO LONG for this model",
		"context length exceeded",
		"context_length exceeded",
		"maximum context reached",
		"unexpected status 413 from provider",
		"some unrelated error",
		"",
		"Too Long",
	}
	for _, s := range errStrings {
		rec(t, "IsOverflow", []any{s}, compaction.IsOverflow(errors.New(s)))
	}
	rec(t, "IsOverflow", []any{nil}, compaction.IsOverflow(nil))
}