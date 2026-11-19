// Package tooloracle records ground-truth outputs of the pure helpers of the external
// github.com/Autumn-27/norma/tool package for the Rust parity harness.
//
// norma/tool lives in the read-only Go module cache, so record.sh cannot copy an oracle into
// it the way the in-repo packages do. Instead this standalone module imports it and prints
// one JSON object per case: {fn, args, ok}. record.sh sorts the lines so the output is
// byte-reproducible. Only exported functions are reachable here.
package tooloracle

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/Autumn-27/norma/tool"
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
	// tool.Text(s) / tool.Errorf(s) — single-text-block results (IsError false / true).
	strs := []string{"", "hello", "line1\nline2", "unicode: café 世界", "Error: something failed"}
	for _, s := range strs {
		rec(t, "Text", []any{s}, mustJSON(tool.Text(s)))
		rec(t, "Errorf", []any{s}, mustJSON(tool.Errorf(s)))
	}

	// tool.RenderDeferredToolsBlock([]string) — sorted name list + fixed instruction block.
	nameSets := [][]string{
		nil,
		{},
		{"only"},
		{"b", "a"},
		{"c", "a", "b"},
		{"x", "x", "a"}, // duplicate kept
		{"SearchExtraTools", "Bash", "ExecuteExtraTool"},
		{"z_tool", "a_tool", "m_tool"},
	}
	for _, ns := range nameSets {
		rec(t, "RenderDeferredToolsBlock", []any{ns}, tool.RenderDeferredToolsBlock(ns))
	}

	// tool.RenderTodoReminder([]tool.Todo) — numbered "[status] content" lines.
	todoSets := [][]tool.Todo{
		nil,
		{},
		{{Content: "do x", Status: tool.TodoPending}},
		{
			{Content: "first", Status: tool.TodoCompleted},
			{Content: "second", Status: tool.TodoInProgress},
			{Content: "third", Status: tool.TodoPending},
		},
		{{Content: "unicode ✓ task", Status: tool.TodoInProgress}},
	}
	for _, ts := range todoSets {
		rec(t, "RenderTodoReminder", []any{mustJSON(ts)}, tool.RenderTodoReminder(ts))
	}
}