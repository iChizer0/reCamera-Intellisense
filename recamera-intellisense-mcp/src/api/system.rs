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
