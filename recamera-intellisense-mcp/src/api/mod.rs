// MARK: Low-level HTTP bindings to the reCamera Record / Model / GPIO APIs.
// Each submodule owns one API family; all HTTP calls go through `api_client`.

pub mod capture;
pub mod daemon;
pub mod gpio;
pub mod model;
pub mod relay;
pub mod rule;
pub mod storage;

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
