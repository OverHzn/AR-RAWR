// Package transcriptoracle records ground-truth outputs of the pure functions of the
// external github.com/Autumn-27/norma/transcript package for the Rust parity harness.
//
// norma/transcript lives in the read-only Go module cache, so record.sh cannot copy an
// oracle into it the way the in-repo packages do. Instead this standalone module imports
// it and prints one JSON object per case: {fn, args, ok}. record.sh sorts the lines so
// the output is byte-reproducible. Only exported functions are reachable here.
package transcriptoracle

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/Autumn-27/norma/llm"
	"github.com/Autumn-27/norma/transcript"
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
func usr(bs ...llm.ContentBlock) llm.Message {
	return llm.Message{Role: llm.RoleUser, Content: bs}
}
func asst(bs ...llm.ContentBlock) llm.Message {
	return llm.Message{Role: llm.RoleAssistant, Content: bs}
}
func msgPtr(m llm.Message) *llm.Message { return &m }
func usrPtr(s string) *llm.Message {
	m := usr(txt(s))
	return &m
}
func asstPtr(s string) *llm.Message {
	m := asst(txt(s))
	return &m
}

func TestZZOracle(t *testing.T) {
	// Each corpus entry is a distinct []transcript.Record. Both Messages and LastUUID are
	// recorded for every entry so the two functions share one fixture set.
	type fixture struct {
		name string
		recs []transcript.Record
	}
	fixtures := []fixture{
		{"nil", nil},                          // Messages -> nil,usage0 ; LastUUID -> ""
		{"empty", []transcript.Record{}},      // Messages -> nil,usage0 ; LastUUID -> ""
		{"single-user", []transcript.Record{
			{UUID: "u1", SessionID: "s", Type: "message", Message: usrPtr("hello")},
		}},
		{"single-asst-usage", []transcript.Record{
			{UUID: "a1", SessionID: "s", Type: "message", Message: asstPtr("hi"), Usage: &llm.Usage{InputTokens: 10, OutputTokens: 5}},
		}},
		{"boundary-only", []transcript.Record{
			{UUID: "b1", SessionID: "s", Type: "boundary"},
		}}, // Message nil + Type boundary -> skipped; Messages nil, LastUUID "b1"
		{"nil-message-skip", []transcript.Record{
			{UUID: "n1", SessionID: "s", Type: "message"}, // Type message but Message nil -> skipped
		}},
		{"usage-without-message", []transcript.Record{
			// Type message, Message nil, but Usage set: Go skips the record entirely so the
			// usage is NOT accumulated.
			{UUID: "w1", SessionID: "s", Type: "message", Usage: &llm.Usage{InputTokens: 99}},
		}},
		{"message-without-usage", []transcript.Record{
			{UUID: "m1", SessionID: "s", Type: "message", Message: usrPtr("no usage here")},
		}},
		{"mixed-chain", []transcript.Record{
			{UUID: "u1", ParentUUID: "", SessionID: "s", Type: "message", Message: usrPtr("q1")},
			{UUID: "a1", ParentUUID: "u1", SessionID: "s", Type: "message", Message: asstPtr("a1"), Usage: &llm.Usage{InputTokens: 12, OutputTokens: 8, CacheReadTokens: 3}},
			{UUID: "b1", ParentUUID: "a1", SessionID: "s", Type: "boundary"},
			{UUID: "u2", ParentUUID: "a1", SessionID: "s", Type: "message", Message: usrPtr("q2")},
		}}, // Messages: u1,a1,u2 ; Usage: {12,8,3,0} ; LastUUID "u2"
		{"usage-accumulate", []transcript.Record{
			{UUID: "a1", SessionID: "s", Type: "message", Message: asstPtr("one"), Usage: &llm.Usage{InputTokens: 1, OutputTokens: 2, CacheReadTokens: 3, CacheWriteTokens: 4}},
			{UUID: "a2", SessionID: "s", Type: "message", Message: asstPtr("two"), Usage: &llm.Usage{InputTokens: 10, OutputTokens: 20, CacheReadTokens: 30, CacheWriteTokens: 40}},
			{UUID: "a3", SessionID: "s", Type: "message", Message: asstPtr("three")}, // nil usage: no add
		}}, // Usage: {11,22,33,44}
		{"all-skipped", []transcript.Record{
			{UUID: "b1", SessionID: "s", Type: "boundary"},
			{UUID: "n1", SessionID: "s", Type: "message"}, // nil message
			{UUID: "b2", SessionID: "s", Type: "boundary", Message: usrPtr("ignored because boundary")},
		}}, // boundary with a message is STILL skipped (Type check first); Messages nil; LastUUID "b2"
		{"trailing-boundary", []transcript.Record{
			{UUID: "u1", SessionID: "s", Type: "message", Message: usrPtr("only msg")},
			{UUID: "b1", ParentUUID: "u1", SessionID: "s", Type: "boundary"},
		}}, // Messages: u1 ; LastUUID "b1" (last record even though boundary)
		{"sidechain-and-agents", []transcript.Record{
			{UUID: "u1", SessionID: "s", AgentID: "main", Type: "message", Message: usrPtr("top")},
			{UUID: "s1", ParentUUID: "u1", SessionID: "s", AgentID: "sub", IsSidechain: true, Type: "message", Message: asstPtr("side"), Usage: &llm.Usage{OutputTokens: 7}},
		}}, // Messages: both (Messages does not filter sidechain) ; Usage {0,7,0,0}
		{"tool-blocks", []transcript.Record{
			{UUID: "a1", SessionID: "s", Type: "message", Message: msgPtr(llm.Message{Role: llm.RoleAssistant, Content: []llm.ContentBlock{
				txt("let me check"),
				{Type: llm.BlockToolUse, ID: "t1", Name: "run", Input: json.RawMessage(`{"cmd":"ls"}`)},
			}}), Usage: &llm.Usage{InputTokens: 5, OutputTokens: 6}},
		}},
	}

	for _, f := range fixtures {
		msgs, usage := transcript.Messages(f.recs)
		rec(t, "Messages", []any{mustJSON(f.recs)}, map[string]any{
			"messages": msgs,
			"usage":    usage,
		})
		rec(t, "LastUUID", []any{mustJSON(f.recs)}, transcript.LastUUID(f.recs))
	}
}