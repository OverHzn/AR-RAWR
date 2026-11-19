// Package permissionoracle records ground-truth outputs of the pure decision logic of
// the external github.com/Autumn-27/norma/permission package for the Rust parity harness.
//
// norma/permission lives in the read-only Go module cache, so record.sh cannot copy an
// oracle into it the way the in-repo packages do. Instead this standalone module imports
// it and prints one JSON object per case: {fn, args, ok}. record.sh sorts the lines so
// the output is byte-reproducible. Only exported functions are reachable here.
//
// The one impure input to Evaluate is the host approval callback (Request.Ask). We model
// it by its already-resolved return value: a fixture with ask==nil leaves Ask unset (Go
// nil callback), and a fixture with a non-nil ask installs a closure returning that exact
// Decision. The recorded `ask` arg carries the resolved Decision (or null) so the Rust
// side can reconstruct the identical scenario and run its pure port.
package permissionoracle

import (
	"context"
	"encoding/json"
	"fmt"
	"testing"

	"github.com/Autumn-27/norma/permission"
)

func rec(t *testing.T, fn string, args []any, out any) {
	t.Helper()
	c := map[string]any{"fn": fn, "args": args, "ok": out}
	b, _ := json.Marshal(c)
	fmt.Println(string(b))
}

type fixture struct {
	tool       string
	readOnly   bool
	toolDec    permission.Decision
	mode       permission.Mode
	allowed    []string
	disallowed []string
	ask        *permission.Decision // nil = no callback; non-nil = resolved return value
}

func TestZZOracle(t *testing.T) {
	allow := permission.Allowed()
	denyHard := permission.Denied("tool self-validation failed")

	// A rich tool Allow verdict exercising the full Decision round-trip (UpdatedInput +
	// Suggestions) when Evaluate returns it verbatim in stage 3.
	richAllow := permission.Decision{
		Behavior:     permission.Allow,
		UpdatedInput: json.RawMessage(`{"path":"x"}`),
		Message:      "ok by tool",
		Suggestions: []permission.Update{
			{Type: "addAllowRule", Rule: "Bash(git:*)"},
			{Type: "addAllowRule", Rule: "Read"},
		},
	}

	fixtures := []fixture{
		// ---- Stage 1: deny rules + tool hard-deny ----
		{tool: "Bash", mode: permission.ModeDefault, disallowed: []string{"Bash"}},
		{tool: "Read", mode: permission.ModeBypass, disallowed: []string{"*"}},                 // deny beats bypass
		{tool: "Bash", mode: permission.ModeDefault, disallowed: []string{"Bash(git:*)"}},      // Name(...) form
		{tool: "Bash", mode: permission.ModeDefault, disallowed: []string{"  Bash  "}},          // trimmed
		{tool: "Bash", mode: permission.ModeDefault, disallowed: []string{"Bashx(git:*)"}},     // prefix != tool -> no match, falls through
		{tool: "Bash", mode: permission.ModeDefault, disallowed: []string{"(git:*)"}},          // '(' at idx 0 -> no match
		{tool: "Read", mode: permission.ModeDefault, disallowed: []string{"Write"}},            // no match -> read-only allow later
		{tool: "Bash", toolDec: denyHard, readOnly: true, mode: permission.ModeDefault},        // hard-deny beats read-only allow
		{tool: "Bash", toolDec: denyHard, mode: permission.ModeBypass},                         // hard-deny beats bypass

		// ---- Stage 2: mode short-circuits ----
		{tool: "Bash", mode: permission.ModeBypass},                        // bypass -> allow
		{tool: "Read", readOnly: true, mode: permission.ModePlan},          // plan + read-only -> allow
		{tool: "Write", readOnly: false, mode: permission.ModePlan},        // plan + non-read-only -> deny
		{tool: "Bash", readOnly: false, mode: permission.ModePlan},         // plan + non-read-only -> deny

		// ---- Stage 3: allow rules + auto-allows ----
		{tool: "Bash", mode: permission.ModeDefault, allowed: []string{"Bash"}},            // bare allow
		{tool: "Bash", mode: permission.ModeDontAsk, allowed: []string{"*"}},               // "*" allow beats dontAsk
		{tool: "Bash", mode: permission.ModeDefault, allowed: []string{"Bash(git:*)"}},     // Name(...) allow
		{tool: "Write", mode: permission.ModeAcceptEdits},                                  // acceptEdits auto-allows Write
		{tool: "Edit", mode: permission.ModeAcceptEdits},                                   // acceptEdits auto-allows Edit
		{tool: "NotebookEdit", mode: permission.ModeAcceptEdits},                           // acceptEdits auto-allows NotebookEdit
		{tool: "Bash", mode: permission.ModeAcceptEdits, allowed: []string{"Read"}},        // acceptEdits + non-edit + no match -> falls through
		{tool: "Read", mode: permission.ModeDefault},                                       // read-only tool -> allow
		{tool: "Bash", readOnly: true, mode: permission.ModeDontAsk},                       // read-only beats dontAsk deny
		{tool: "Bash", toolDec: allow, readOnly: false, mode: permission.ModeDefault},      // tool Allow verdict returned (stage 3)
		{tool: "Bash", toolDec: richAllow, readOnly: false, mode: permission.ModeDefault},  // tool Allow verdict, full round-trip

		// ---- Stage 4: interactive confirmation ----
		{tool: "Bash", mode: permission.ModeDontAsk},                                       // dontAsk -> deny
		{tool: "Bash", mode: permission.ModeAuto},                                          // auto -> deny
		{tool: "Bash", mode: permission.ModeDefault},                                       // no ask -> deny (no handler)
		{tool: "Bash", mode: permission.ModeDefault, ask: &allow},                          // ask returns Allow -> verbatim
		{tool: "Bash", mode: permission.ModeDefault, ask: func() *permission.Decision {
			d := permission.Denied("user said no")
			return &d
		}()}, // ask returns Deny -> verbatim
		{tool: "Bash", mode: permission.ModeDefault, ask: func() *permission.Decision {
			d := permission.AskUser("confirm?")
			return &d
		}()}, // ask returns Ask -> verbatim
		{tool: "Bash", mode: permission.ModeDefault, ask: &permission.Decision{}}, // ask returns empty -> "denied by approval callback"
	}

	for _, f := range fixtures {
		req := permission.Request{
			ToolName:     f.tool,
			IsReadOnly:   f.readOnly,
			ToolDecision: f.toolDec,
			Ctx: permission.Context{
				Mode:       f.mode,
				Allowed:    f.allowed,
				Disallowed: f.disallowed,
			},
		}
		if f.ask != nil {
			d := *f.ask
			req.Ask = func(context.Context, string, json.RawMessage, permission.Context) permission.Decision {
				return d
			}
		}
		out := permission.Evaluate(context.Background(), req)

		var askArg any = nil
		if f.ask != nil {
			askArg = *f.ask
		}
		rec(t, "Evaluate", []any{
			map[string]any{
				"tool":         f.tool,
				"readOnly":     f.readOnly,
				"toolDecision": f.toolDec,
				"mode":         string(f.mode),
				"allowed":      f.allowed,
				"disallowed":   f.disallowed,
				"ask":          askArg,
			},
		}, out)
	}

	// Constructors, recorded directly.
	rec(t, "Allowed", []any{}, permission.Allowed())
	rec(t, "Denied", []any{"denied: because"}, permission.Denied("denied: because"))
	rec(t, "Denied", []any{""}, permission.Denied(""))
	rec(t, "AskUser", []any{"allow this?"}, permission.AskUser("allow this?"))
	rec(t, "AskUser", []any{""}, permission.AskUser(""))
}