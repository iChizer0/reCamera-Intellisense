//! Video stream encode settings (`/video/{0,1}/encode`).

use anyhow::{bail, Result};
use serde_json::Value;

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, SetVideoEncodeParams, VideoEncode, VideoEncodeSetResult};

fn stream_key_id(stream: &str) -> Result<(String, i32)> {
    let key = stream.trim().to_lowercase();
    let id = match key.as_str() {
        "main" => 0,
        "sub" => 1,
        _ => bail!("stream must be 'main' or 'sub'; got {stream:?}"),
    };
    Ok((key, id))
}

fn encode_path(stream: &str) -> Result<String> {
    let (_, id) = stream_key_id(stream)?;
    Ok(format!("/cgi-bin/entry.cgi/video/{id}/encode"))
}

/// Encode parameters of one stream (`main` = 0, `sub` = 1).
pub async fn get_video_encode(
    client: &ApiClient,
    device: &DeviceRecord,
    stream: &str,
) -> Result<VideoEncode> {
    let (key, _) = stream_key_id(stream)?;
    let d = client.get_json(device, &encode_path(stream)?, None).await?;
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

const CODECS: [&str; 2] = ["H.264", "H.265"];
const RC_MODES: [&str; 2] = ["CBR", "VBR"];
const RC_QUALITIES: [&str; 4] = ["highest", "high", "medium", "low"];

fn norm_resolution(value: &str) -> Result<String> {
    let text = value.to_lowercase().replace('x', "*");
    let (w, h) = text
        .split_once('*')
        .and_then(|(a, b)| Some((a.parse::<i64>().ok()?, b.parse::<i64>().ok()?)))
        .ok_or_else(|| anyhow::anyhow!("resolution must be \"WxH\" within 384*384 ~ 3840*2160"))?;
    if !(384..=3840).contains(&w) || !(384..=2160).contains(&h) {
        bail!("resolution must be \"WxH\" within 384*384 ~ 3840*2160");
    }
    Ok(format!("{w}*{h}"))
}

/// Build the wire payload + the friendly verify map. Pure for unit tests.
fn build_encode_payload(
    params: &SetVideoEncodeParams,
) -> Result<(serde_json::Map<String, Value>, serde_json::Map<String, Value>)> {
    let mut payload = serde_json::Map::new();
    let mut checks = serde_json::Map::new();
    if let Some(codec) = &params.codec {
        let codec = codec.to_uppercase();
        if !CODECS.contains(&codec.as_str()) {
            bail!("codec must be one of {CODECS:?}");
        }
        payload.insert("sOutputDataType".into(), codec.clone().into());
        checks.insert("codec".into(), codec.into());
    }
    if let Some(resolution) = &params.resolution {
        let norm = norm_resolution(resolution)?;
        payload.insert("sResolution".into(), norm.clone().into());
        checks.insert("resolution".into(), norm.into());
    }
    if let Some(fps) = params.frame_rate {
        if !(1..=120).contains(&fps) {
            bail!("frame_rate must be 1~120");
        }
        payload.insert("sFrameRate".into(), fps.to_string().into()); // numeric STRING on the wire
        checks.insert("frame_rate".into(), fps.to_string().into());
    }
    if let Some(gop) = params.gop {
        if !(1..=120).contains(&gop) {
            bail!("gop must be 1~120");
        }
        payload.insert("iGOP".into(), gop.into());
        checks.insert("gop".into(), gop.into());
    }
    if let Some(rc_mode) = &params.rc_mode {
        let mode = rc_mode.to_uppercase();
        if !RC_MODES.contains(&mode.as_str()) {
            bail!("rc_mode must be one of {RC_MODES:?}");
        }
        payload.insert("sRCMode".into(), mode.clone().into());
        checks.insert("rc_mode".into(), mode.into());
    }
    if let Some(rc_quality) = &params.rc_quality {
        let quality = rc_quality.to_lowercase();
        if !RC_QUALITIES.contains(&quality.as_str()) {
            bail!("rc_quality must be one of {RC_QUALITIES:?}");
        }
        payload.insert("sRCQuality".into(), quality.clone().into());
        checks.insert("rc_quality".into(), quality.into());
    }
    if let Some(max_rate) = params.max_rate {
        if !(3..=65536).contains(&max_rate) {
            bail!("max_rate must be 3~65536");
        }
        payload.insert("iMaxRate".into(), max_rate.into());
        checks.insert("max_rate".into(), max_rate.into());
    }
    if let Some(enabled) = params.enabled {
        payload.insert("iEnabled".into(), (enabled as i64).into());
        checks.insert("enabled".into(), enabled.into());
    }
    if payload.is_empty() {
        bail!(
            "nothing to change: pass codec, resolution, frame_rate, gop, rc_mode, \
             rc_quality, max_rate, or enabled"
        );
    }
    Ok((payload, checks))
}

/// Update encode parameters; only passed fields change, and the result is
/// verified by re-reading the device. Applying briefly re-inits the encoder.
pub async fn set_video_encode(
    client: &ApiClient,
    device: &DeviceRecord,
    params: &SetVideoEncodeParams,
) -> Result<VideoEncodeSetResult> {
    let (stream, _) = stream_key_id(params.stream.as_deref().unwrap_or("main"))?;
    let (payload, checks) = build_encode_payload(params)?;
    let resp = client
        .post_json(
            device,
            &encode_path(&stream)?,
            None,
            Some(&Value::Object(payload)),
        )
        .await?;
    expect_ok(&resp, "set video encode")?;
    let after = get_video_encode(client, device, &stream).await?;
    let after_value = serde_json::to_value(&after)?;
    let mismatched: Vec<String> = checks
        .iter()
        .filter(|(k, v)| {
            after_value
                .get(k.as_str())
                .map(|got| got.to_string().trim_matches('"') != v.to_string().trim_matches('"'))
                .unwrap_or(true)
        })
        .map(|(k, v)| format!("{k}: wanted {v}, got {:?}", after_value.get(k.as_str())))
        .collect();
    if !mismatched.is_empty() {
        bail!("set video encode did not apply: {}", mismatched.join(", "));
    }
    Ok(VideoEncodeSetResult {
        changed: true,
        stream,
        applied: Value::Object(checks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> SetVideoEncodeParams {
        SetVideoEncodeParams {
            device_name: String::new(),
            stream: None,
            codec: None,
            resolution: None,
            frame_rate: None,
            gop: None,
            rc_mode: None,
            rc_quality: None,
            max_rate: None,
            enabled: None,
        }
    }

    #[test]
    fn nothing_to_change_rejected() {
        assert!(build_encode_payload(&params()).is_err());
    }

    #[test]
    fn wire_types_and_normalization() {
        let mut p = params();
        p.codec = Some("H.265".into());
        p.resolution = Some("1920x1080".into());
        p.frame_rate = Some(25);
        p.rc_mode = Some("vbr".into());
        p.rc_quality = Some("HIGH".into());
        p.enabled = Some(true);
        let (payload, checks) = build_encode_payload(&p).unwrap();
        assert_eq!(payload["sFrameRate"], Value::from("25")); // string
        assert_eq!(payload["iEnabled"], Value::from(1)); // int
        assert_eq!(payload["sResolution"], Value::from("1920*1080"));
        assert_eq!(checks["rc_mode"], Value::from("VBR"));
        assert_eq!(checks["rc_quality"], Value::from("high"));
    }

    #[test]
    fn codec_case_normalized() {
        let mut p = params();
        p.codec = Some("h.265".into());
        let (payload, _) = build_encode_payload(&p).unwrap();
        assert_eq!(payload["sOutputDataType"], Value::from("H.265"));
    }

    #[test]
    fn out_of_range_rejected() {
        for (f, v) in [("frame_rate", 0), ("frame_rate", 121), ("gop", 0), ("max_rate", 2)] {
            let mut p = params();
            match f {
                "frame_rate" => p.frame_rate = Some(v),
                "gop" => p.gop = Some(v),
                _ => p.max_rate = Some(v),
            }
            assert!(build_encode_payload(&p).is_err(), "{f}={v}");
        }
        let mut p = params();
        p.codec = Some("VP9".into());
        assert!(build_encode_payload(&p).is_err());
        let mut p = params();
        p.resolution = Some("100*100".into());
        assert!(build_encode_payload(&p).is_err());
    }
}
