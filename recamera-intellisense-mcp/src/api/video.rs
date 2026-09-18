//! Video stream encode settings (`/video/{0,1}/encode`). Read surface only.

use anyhow::{bail, Result};

use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, VideoEncode};

/// Encode parameters of one stream (`main` = 0, `sub` = 1).
pub async fn get_video_encode(
    client: &ApiClient,
    device: &DeviceRecord,
    stream: &str,
) -> Result<VideoEncode> {
    let key = stream.trim().to_lowercase();
    let id = match key.as_str() {
        "main" => 0,
        "sub" => 1,
        _ => bail!("stream must be 'main' or 'sub'; got {stream:?}"),
    };
    let d = client
        .get_json(device, &format!("/cgi-bin/entry.cgi/video/{id}/encode"), None)
        .await?;
    let text = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok(VideoEncode {
        stream: key,
        stream_type: text("sStreamType"),
        enabled: d.get("iEnabled").and_then(|v| v.as_i64()).unwrap_or(0) != 0,
        codec: text("sOutputDataType"),
        resolution: text("sResolution"),
        frame_rate: text("sFrameRate"),
        gop: d.get("iGOP").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        rc_mode: text("sRCMode"),
        rc_quality: text("sRCQuality"),
        max_rate: d.get("iMaxRate").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
    })
}
