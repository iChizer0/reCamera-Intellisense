use anyhow::Result;
use serde_json::{json, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::DeviceRecord;

// MARK: System CGI

const PATH_DEVICE_INFO: &str = "/cgi-bin/entry.cgi/system/device-info";
const PATH_RESOURCE_INFO: &str = "/cgi-bin/entry.cgi/system/resource-info";
const PATH_TIME: &str = "/cgi-bin/entry.cgi/system/time";
const PATH_REBOOT: &str = "/cgi-bin/entry.cgi/system/reboot";
const PATH_BATTERY: &str = "/cgi-bin/entry.cgi/system/battery";

pub struct SetSystemTimeArgs {
    pub method: String,
    pub timezone: Option<String>,
    pub timestamp: Option<i64>,
    pub ntp_server: Option<String>,
    pub ntp_port: Option<u16>,
}

fn device_info_from(d: &Value) -> Value {
    json!({
        "serial_number": d.get("sSerialNumber"),
        "firmware_version": d.get("sFirmwareVersion"),
        "sensor_model": d.get("sSensorModel"),
        "base_plate_model": d.get("sBasePlateModel"),
    })
}

fn usage_block(d: &Value, total: &str, used: &str, pct: &str) -> Value {
    json!({
        "total_gb": d.get(total),
        "used_gb": d.get(used),
        "usage_percent": d.get(pct),
    })
}

fn resource_info_from(d: &Value) -> Value {
    let empty = json!({});
    let mem = d.get("sMem").unwrap_or(&empty);
    let storage = d.get("sStorage").unwrap_or(&empty);
    json!({
        "cpu_usage": d.get("iCpuUsage"),
        "npu_usage": d.get("iNpuUsage"),
        "memory": usage_block(mem, "iMemTotal", "iMemUsed", "iMemUsage"),
        "storage": usage_block(storage, "iStorageTotal", "iStorageUsed", "iStorageUsage"),
    })
}

fn system_time_from(d: &Value) -> Value {
    let empty = json!({});
    let ntp = d.get("dNtpConfig").unwrap_or(&empty);
    json!({
        "method": d.get("sMethod"),
        "timestamp": d.get("iTimestamp"),
        "timezone": d.get("sTimezone"),
        "tz": d.get("sTz"),
        "ntp": {"address": ntp.get("sAddress"), "port": ntp.get("sPort")},
    })
}

pub async fn get_device_info(client: &ApiClient, device: &DeviceRecord) -> Result<Value> {
    let d = client.get_json(device, PATH_DEVICE_INFO, None).await?;
    Ok(device_info_from(&d))
}

pub async fn get_resource_info(client: &ApiClient, device: &DeviceRecord) -> Result<Value> {
    let d = client.get_json(device, PATH_RESOURCE_INFO, None).await?;
    Ok(resource_info_from(&d))
}

pub async fn get_system_time(client: &ApiClient, device: &DeviceRecord) -> Result<Value> {
    let d = client.get_json(device, PATH_TIME, None).await?;
    Ok(system_time_from(&d))
}

/// Set the device clock / timezone / NTP config, then read back and verify.
///
/// `method="ntp"` applies the NTP server config (keeping the current one
/// unless overridden) and triggers an immediate sync on the device.
/// `method="manual"` sets the clock to `timestamp` (defaults to now).
/// `timezone` takes an IANA name; `None` keeps the current one.
///
/// The firmware reports success even when it silently rejects an unknown
/// timezone, so the device is re-read afterwards: a mismatch is a loud error,
/// not a false success.
pub async fn set_system_time(
    client: &ApiClient,
    device: &DeviceRecord,
    args: SetSystemTimeArgs,
) -> Result<Value> {
    let method = args.method.trim().to_ascii_lowercase();
    anyhow::ensure!(
        matches!(method.as_str(), "ntp" | "manual"),
        "Invalid method {:?}: expected ntp|manual.",
        args.method
    );
    let current = client.get_json(device, PATH_TIME, None).await?;
    let cur_ntp = current.get("dNtpConfig").cloned().unwrap_or(json!({}));

    let tz = match &args.timezone {
        Some(tz) => {
            let tz = tz.trim().to_string();
            anyhow::ensure!(
                super::clock::valid_timezone_name(&tz),
                "Invalid timezone {tz:?}: expected an IANA name like 'Asia/Shanghai'."
            );
            tz
        }
        None => current
            .get("sTimezone")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    };
    anyhow::ensure!(
        !tz.is_empty(),
        "No timezone configured on the device; pass timezone."
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ts = args.timestamp.unwrap_or(now);
    anyhow::ensure!(ts > 0, "Invalid timestamp {ts}: expected unix seconds > 0.");

    let ntp_cfg = if method == "manual" {
        Value::Null
    } else {
        let address = args
            .ntp_server
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| cur_ntp.get("sAddress").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_else(|| "pool.ntp.org".to_string());
        let port = args
            .ntp_port
            .map(|p| p.to_string())
            .or_else(|| cur_ntp.get("sPort").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_else(|| "123".to_string());
        json!({"sAddress": address, "sPort": port})
    };

    let payload = json!({
        "sMethod": method,
        "dNtpConfig": ntp_cfg,
        "iTimestamp": ts,
        "sTimezone": tz,
    });
    let resp = client.put_json(device, PATH_TIME, Some(&payload)).await?;
    expect_ok(&resp, "set system time")?;

    // Read-back verification — the CGI returns success for a rejected timezone.
    let after = client.get_json(device, PATH_TIME, None).await?;
    let mut problems: Vec<String> = Vec::new();
    if args.timezone.is_some() && after.get("sTimezone").and_then(|v| v.as_str()) != Some(tz.as_str()) {
        problems.push(format!(
            "device kept timezone {:?} instead of {tz:?} (unknown or unsupported IANA \
             name — pick one from the Web Console list)",
            after.get("sTimezone").and_then(|v| v.as_str())
        ));
    }
    if method == "manual" {
        let drift = (after
            .get("iTimestamp")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            - ts)
            .abs();
        if drift > 5 {
            problems.push(format!(
                "device clock is {drift}s away from the requested timestamp"
            ));
        }
    }
    anyhow::ensure!(
        problems.is_empty(),
        "set_system_time was applied but verification failed: {}",
        problems.join("; ")
    );
    let mut out = system_time_from(&after);
    out["verified"] = json!(true);
    Ok(out)
}

pub async fn reboot_device(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let resp = client.post_json(device, PATH_REBOOT, None, None).await?;
    expect_ok(&resp, "reboot device")
}

// MARK: Tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_info_normalizes() {
        let info = device_info_from(&json!({
            "sSerialNumber": "unknown", "sFirmwareVersion": "V1.1.1",
            "sSensorModel": "SC850SL", "sBasePlateModel": "Base Board-V1.0",
        }));
        assert_eq!(info["firmware_version"], "V1.1.1");
        assert_eq!(info["sensor_model"], "SC850SL");
    }

    #[test]
    fn resource_info_normalizes_nested_blocks() {
        let info = resource_info_from(&json!({
            "iCpuUsage": 5, "iNpuUsage": 20,
            "sMem": {"iMemTotal": 1.94, "iMemUsage": 30, "iMemUsed": 0.59},
            "sStorage": {"iStorageTotal": 11.29, "iStorageUsage": 38, "iStorageUsed": 4.35},
        }));
        assert_eq!(info["npu_usage"], 20);
        assert_eq!(
            info["memory"],
            json!({"total_gb": 1.94, "used_gb": 0.59, "usage_percent": 30})
        );
        assert_eq!(info["storage"]["usage_percent"], 38);
    }

    #[test]
    fn system_time_normalizes() {
        let t = system_time_from(&json!({
            "dNtpConfig": {"sAddress": "pool.ntp.org", "sPort": "123", "status": 0},
            "iTimestamp": 1787220510, "sMethod": "ntp", "sTimezone": "UTC", "sTz": "UTC+0",
        }));
        assert_eq!(t["ntp"], json!({"address": "pool.ntp.org", "port": "123"}));
        assert_eq!(t["tz"], "UTC+0");
    }
}

/// Battery/power status. `attached` is false on base plates without a battery.
pub async fn get_battery_status(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<crate::types::BatteryStatus> {
    let d = client.get_json(device, PATH_BATTERY, None).await?;
    Ok(crate::types::BatteryStatus {
        attached: d.get("isAttached").and_then(|v| v.as_bool()).unwrap_or(false),
        charging: d.get("isCharging").and_then(|v| v.as_bool()).unwrap_or(false),
        display_steps: d.get("displaySteps").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        total_steps: d.get("totalSteps").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
    })
}
