//! Built-in browser (Playwright) MCP seeding + the one-time row migration —
//! port of `db/browser_mcp.go`.
//!
//! A fresh database gets a single `browser` MCP row backed by the image's global
//! `playwright-mcp` binary (NOT `npx @playwright/mcp`, which asks the npm
//! registry on every start), launched headless against Playwright's bundled
//! Chromium (the default `chrome` channel the image does not ship). The row is
//! inserted disabled with no proxy; the traffic-capture toggle injects the
//! recording proxy + CA at runtime.
//!
//! `migrateStockBrowserMCP` is the one-time upgrade for databases created by an
//! older build: a leftover `npx @playwright/mcp` row becomes `playwright-mcp`,
//! and a `playwright-mcp` row that still has no `--browser` gets `--browser
//! chromium`. It is gated by a settings flag so an operator who later puts `npx`
//! back is not rewritten on the next start, and any other command/package is
//! left untouched.

use crate::db::error::{Error, Result};
use crate::db::DB;

/// `browserMCPName` — the fixed name of the built-in browser MCP row.
pub(crate) const BROWSER_MCP_NAME: &str = "browser";
/// `browserMCPCommand` — the installed binary the row launches.
pub(crate) const BROWSER_MCP_COMMAND: &str = "playwright-mcp";
/// `browserMCPDirectBinSetting` — gates the one-time npx→binary migration.
pub(crate) const BROWSER_MCP_DIRECT_BIN_SETTING: &str = "browser_mcp_direct_bin_v1";
/// `browserMCPSeedArgs` — the JSON args array for a fresh row.
pub(crate) const BROWSER_MCP_SEED_ARGS: &str = r#"["--headless","--browser","chromium"]"#;

/// `npxMetaFlags` — npx switches that take no value and leave the following
/// argument as the package spec. Flags that take a value (`-p`, `--package`) are
/// treated as a custom invocation and left alone.
fn is_npx_meta_flag(a: &str) -> bool {
    matches!(a, "-y" | "--yes" | "--" | "-q" | "--quiet")
}

/// `playwrightMCPPackageIndex` — the index of the `@playwright/mcp` package spec
/// in `args`, skipping leading npx meta flags. `None` when the first real
/// argument is not that package.
fn playwright_mcp_package_index(args: &[String]) -> Option<usize> {
    for (i, a) in args.iter().enumerate() {
        if is_npx_meta_flag(a) {
            continue;
        }
        if a == "@playwright/mcp" || a.starts_with("@playwright/mcp@") {
            return Some(i);
        }
        return None;
    }
    None
}

/// `rewriteStockNpxBrowserMCP` — convert `npx [@meta...] @playwright/mcp [...]`
/// into the global `playwright-mcp` binary with `--headless`/`--browser chromium`
/// filled in. `ok=false` (inputs returned unchanged) when the row is not that
/// invocation.
fn rewrite_stock_npx_browser_mcp(command: &str, args: &[String]) -> (String, Vec<String>, bool) {
    if command != "npx" {
        return (command.to_string(), args.to_vec(), false);
    }
    let Some(idx) = playwright_mcp_package_index(args) else {
        return (command.to_string(), args.to_vec(), false);
    };
    let mut rest: Vec<String> = args[idx + 1..].to_vec();
    if !has_cli_flag(&rest, "--headless") {
        rest.push("--headless".to_string());
    }
    if !has_cli_flag(&rest, "--browser") {
        rest.push("--browser".to_string());
        rest.push("chromium".to_string());
    }
    (BROWSER_MCP_COMMAND.to_string(), rest, true)
}

/// `rewriteBrowserMCPRow` — the one-time row rewrite: stock npx `@playwright/mcp`
/// becomes `playwright-mcp`, and a `playwright-mcp` row that still has no
/// `--browser` gets `--browser chromium`. Any other command/package is returned
/// unchanged with `ok=false`.
fn rewrite_browser_mcp_row(command: &str, args: &[String]) -> (String, Vec<String>, bool) {
    let (mut cmd, mut out, mut ok) = rewrite_stock_npx_browser_mcp(command, args);
    if !ok {
        cmd = command.to_string();
        out = args.to_vec();
    }
    if cmd == BROWSER_MCP_COMMAND && !has_cli_flag(&out, "--browser") {
        out.push("--browser".to_string());
        out.push("chromium".to_string());
        ok = true;
    }
    (cmd, out, ok)
}

/// `hasCLIFlag` — does `args` contain `flag` (exact) or `flag=...`?
fn has_cli_flag(args: &[String], flag: &str) -> bool {
    let eq = format!("{flag}=");
    args.iter().any(|a| a == flag || a.starts_with(&eq))
}

impl DB {
    /// `migrateStockBrowserMCP`: rewrite a leftover stock `npx @playwright/mcp`
    /// browser MCP row once, and add `--browser chromium` to a `playwright-mcp`
    /// row that still has none. Gated by a settings flag; any other row is left
    /// as-is. Always marks the flag done (even when there is nothing to do).
    pub(crate) async fn migrate_stock_browser_mcp(&self) -> Result<()> {
        if self.get_setting(BROWSER_MCP_DIRECT_BIN_SETTING).await?.0 == "done" {
            return Ok(());
        }
        let conn = self.pool.get().await?;
        let row = conn
            .query_opt(
                "SELECT COALESCE(command,''), args FROM mcp_servers WHERE name=$1",
                &[&BROWSER_MCP_NAME],
            )
            .await?;
        let Some(row) = row else {
            return self
                .set_setting(BROWSER_MCP_DIRECT_BIN_SETTING, "done")
                .await;
        };
        let command: String = row.get(0);
        let args_val: serde_json::Value = row.get(1);

        // Decode the JSONB args array into a Vec<String>; anything that is not an
        // array of strings is treated as "nothing to migrate" (Go json.Unmarshal
        // into []string fails the same way).
        let args = match decode_string_array(&args_val) {
            Some(a) => a,
            None => {
                return self
                    .set_setting(BROWSER_MCP_DIRECT_BIN_SETTING, "done")
                    .await
            }
        };

        let (new_command, new_args, ok) = rewrite_browser_mcp_row(&command, &args);
        if ok {
            let raw = serde_json::Value::from(new_args);
            conn.execute(
                "UPDATE mcp_servers SET command=$1, args=$2 WHERE name=$3",
                &[&new_command, &raw, &BROWSER_MCP_NAME],
            )
            .await
            .map_err(|e| Error::msg(format!("update browser mcp: {e}")))?;
        }
        self.set_setting(BROWSER_MCP_DIRECT_BIN_SETTING, "done")
            .await
    }
}

/// Decode a JSONB value into `Vec<String>` iff it is an array of strings.
fn decode_string_array(v: &serde_json::Value) -> Option<Vec<String>> {
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        out.push(e.as_str()?.to_string());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rewrite_stock_npx_browser_mcp_cases() {
        // (command, args, want_ok, want_command, want_args)
        type Case<'a> = (&'a str, &'a [&'a str], bool, &'a str, &'a [&'a str]);
        let cases: &[Case] = &[
            (
                "npx",
                &["@playwright/mcp", "--headless"],
                true,
                "playwright-mcp",
                &["--headless", "--browser", "chromium"],
            ),
            (
                "npx",
                &["@playwright/mcp"],
                true,
                "playwright-mcp",
                &["--headless", "--browser", "chromium"],
            ),
            (
                "npx",
                &["@playwright/mcp@0.0.83", "--headless"],
                true,
                "playwright-mcp",
                &["--headless", "--browser", "chromium"],
            ),
            (
                "npx",
                &[
                    "@playwright/mcp",
                    "--headless",
                    "--proxy-server",
                    "http://127.0.0.1:8788",
                ],
                true,
                "playwright-mcp",
                &[
                    "--headless",
                    "--proxy-server",
                    "http://127.0.0.1:8788",
                    "--browser",
                    "chromium",
                ],
            ),
            (
                "npx",
                &["@playwright/mcp", "--headless", "--browser", "firefox"],
                true,
                "playwright-mcp",
                &["--headless", "--browser", "firefox"],
            ),
            (
                "npx",
                &["@playwright/mcp", "--browser=chrome"],
                true,
                "playwright-mcp",
                &["--browser=chrome", "--headless"],
            ),
            (
                "npx",
                &["-y", "@playwright/mcp", "--headless"],
                true,
                "playwright-mcp",
                &["--headless", "--browser", "chromium"],
            ),
            (
                "npx",
                &["--yes", "--", "@playwright/mcp"],
                true,
                "playwright-mcp",
                &["--headless", "--browser", "chromium"],
            ),
            // already playwright-mcp: not an npx row, unchanged
            (
                "playwright-mcp",
                &["--headless"],
                false,
                "playwright-mcp",
                &["--headless"],
            ),
            // npx -p is a custom invocation: first real arg is "-p", not the package
            (
                "npx",
                &["-p", "@playwright/mcp", "playwright-mcp", "--headless"],
                false,
                "npx",
                &["-p", "@playwright/mcp", "playwright-mcp", "--headless"],
            ),
            (
                "npx",
                &["@modelcontextprotocol/server-github"],
                false,
                "npx",
                &["@modelcontextprotocol/server-github"],
            ),
            ("npx", &[], false, "npx", &[]),
            ("", &[], false, "", &[]),
        ];
        for (cmd, args, want_ok, want_cmd, want_args) in cases {
            let args_v = s(args);
            let (gc, ga, ok) = rewrite_stock_npx_browser_mcp(cmd, &args_v);
            assert_eq!(ok, *want_ok, "ok for {cmd} {args:?}");
            if !*want_ok {
                assert_eq!(gc, *cmd, "unchanged command for {cmd} {args:?}");
                assert_eq!(ga, args_v, "unchanged args for {cmd} {args:?}");
                continue;
            }
            assert_eq!(gc, *want_cmd, "command for {cmd} {args:?}");
            assert_eq!(ga, s(want_args), "args for {cmd} {args:?}");
        }
    }

    #[test]
    fn rewrite_browser_mcp_row_cases() {
        let (cmd, args, ok) = rewrite_browser_mcp_row("playwright-mcp", &s(&["--headless"]));
        assert!(ok && cmd == BROWSER_MCP_COMMAND);
        assert_eq!(args, s(&["--headless", "--browser", "chromium"]));

        let (_cmd, _args, ok) = rewrite_browser_mcp_row(
            "playwright-mcp",
            &s(&["--headless", "--browser", "firefox"]),
        );
        assert!(!ok, "existing --browser must not be rewritten");

        let (cmd, args, ok) =
            rewrite_browser_mcp_row("npx", &s(&["-y", "@playwright/mcp", "--headless"]));
        assert!(ok && cmd == BROWSER_MCP_COMMAND);
        assert_eq!(args, s(&["--headless", "--browser", "chromium"]));
    }

    #[test]
    fn rewrite_does_not_alias_input() {
        let input = s(&["@playwright/mcp", "--headless"]);
        let orig = input.clone();
        let (_c, mut out, ok) = rewrite_stock_npx_browser_mcp("npx", &input);
        assert!(ok);
        out[0] = "mutated".to_string();
        assert_eq!(input, orig, "input slice was mutated");
    }
}
