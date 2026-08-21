// MARK: Low-level HTTP bindings to the reCamera Record / Model / GPIO APIs.
// Each submodule owns one API family; all HTTP calls go through `api_client`.

pub mod acoustic;
pub mod capture;
pub mod daemon;
pub mod gpio;
pub mod image;
pub mod model;
pub mod relay;
pub mod rule;
pub mod storage;
pub mod system;

use anyhow::{bail, Result};
use serde_json::Value;

/// Validate the `code == 0` contract shared by all Record API POSTs.
pub(crate) fn expect_ok(resp: &Value, context: &str) -> Result<()> {
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 {
        return Ok(());
    }
    let msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown error");
    bail!("{context} failed (code={code}): {msg}");
}

/// Gate destructive operations: refuse unless explicitly confirmed.
pub fn require_confirm(confirm: bool, what: &str) -> Result<()> {
    if !confirm {
        bail!(
            "{what} is destructive and was NOT executed. \
             Re-run with confirm=true only after explicit user approval."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn confirm_gate_blocks_until_confirmed() {
        let err = super::require_confirm(false, "reboot device").unwrap_err();
        assert!(err.to_string().contains("confirm=true"));
        assert!(super::require_confirm(true, "reboot device").is_ok());
    }
}
