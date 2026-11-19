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

/// `npxMetaFlags`, `playwrightMCPPackageIndex`, `rewriteStockNpxBrowserMCP`,
/// `rewriteBrowserMCPRow`, `hasCLIFlag` — the side-effect-free rewrite logic is
/// parity-proven in `dbx-rs` (single source of truth; the gate keeps it pinned to
/// the real Go functions). We delegate to it here rather than carry a hand-port.
use dbx_rs::port::rewrite_browser_mcp_row;

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
