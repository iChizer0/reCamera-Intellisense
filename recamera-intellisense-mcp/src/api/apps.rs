//! App Center applications (`/api/app-center/v1/apps`): listing and logs.

use anyhow::{bail, Result};

use crate::api_client::ApiClient;
use crate::types::{AppEntry, AppLogs, DeviceRecord};

const PATH_APPS: &str = "/api/app-center/v1/apps";
const MAX_LOG_TAIL: i64 = 2000;

/// Mirrors the appmgr route guard; app_id is URL-path interpolated.
fn require_app_id(app_id: &str) -> Result<()> {
    let ok = !app_id.is_empty()
        && app_id.len() <= 64
        && app_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!("invalid app_id {app_id:?}: expected [a-z0-9-]{{1,64}} (see list_apps)");
    }
    Ok(())
}

/// List App Center applications (installed apps plus firmware system apps
/// such as `builtin` and `acousticslab`), normalized.
pub async fn list_apps(client: &ApiClient, device: &DeviceRecord) -> Result<Vec<AppEntry>> {
    let data = client.get_json(device, PATH_APPS, None).await?;
    let mut out = Vec::new();
    for app in data.get("apps").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let id = app.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let manifest = app.get("manifest").cloned().unwrap_or_default();
        let mtext = |key: &str| {
            app.get(key)
                .and_then(|v| v.as_str())
                .or_else(|| manifest.get(key).and_then(|v| v.as_str()))
                .map(String::from)
        };
        let ty = app.get("type").and_then(|v| v.as_str()).unwrap_or("");
        out.push(AppEntry {
            id: id.to_string(),
            name: mtext("name").unwrap_or_else(|| id.to_string()),
            name_zh: mtext("name_zh"),
            version: mtext("version"),
            status: app.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            system: app.get("system").and_then(|v| v.as_bool()).unwrap_or(false)
                || ty == "system"
                || ty == "builtin",
            installed: app.get("installed").and_then(|v| v.as_bool()).unwrap_or(true),
            description: mtext("description").unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Recent app log lines; `tail` clamps to [1, 2000], spanning the rotated log.
pub async fn get_app_logs(
    client: &ApiClient,
    device: &DeviceRecord,
    app_id: &str,
    tail: i64,
) -> Result<AppLogs> {
    require_app_id(app_id)?;
    let tail = tail.clamp(1, MAX_LOG_TAIL);
    let endpoint = format!("{PATH_APPS}/{app_id}/logs");
    let tail_str = tail.to_string();
    let data = client
        .get_json(device, &endpoint, Some(&[("tail", tail_str.as_str())]))
        .await?;
    Ok(AppLogs {
        id: data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(app_id)
            .to_string(),
        lines: data
            .get("lines")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_validation_mirrors_the_route_guard() {
        assert!(require_app_id("acousticslab").is_ok());
        assert!(require_app_id("my-app-1").is_ok());
        for bad in ["../etc", "UPPER", "with space", "", "under_score"] {
            assert!(require_app_id(bad).is_err(), "{bad}");
        }
        assert!(require_app_id(&"x".repeat(65)).is_err());
    }
}
