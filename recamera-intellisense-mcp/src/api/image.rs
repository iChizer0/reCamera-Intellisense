use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::DeviceRecord;

// MARK: ISP CGI (/image/0) — one table drives normalization and validation

const PATH_IMAGE: &str = "/cgi-bin/entry.cgi/image/0";
const SCENES: [i64; 3] = [0, 1, 2]; // 0=general, 1=day, 2=night
const OPEN_CLOSE: &[&str] = &["open", "close"];

#[derive(Clone, Copy)]
enum FieldKind {
    StrEnum(&'static [&'static str]),
    IntEnum(&'static [i64]),
    IntRange(i64, i64),
    Fraction,
}

struct FieldSpec {
    friendly: &'static str,
    key: &'static str,
    kind: FieldKind,
}

struct SectionSpec {
    name: &'static str,
    device_key: &'static str,
    path: &'static str, // full PUT path; '{scene}' substituted for profile sections
    profile: bool,
    fields: &'static [FieldSpec],
    extras: &'static [(&'static str, &'static str)], // read-only device_key -> friendly
}

const fn pct(friendly: &'static str, key: &'static str) -> FieldSpec {
    FieldSpec { friendly, key, kind: FieldKind::IntRange(0, 100) }
}

const PROFILE_SECTIONS: [&str; 5] = ["adjustment", "exposure", "backlight", "white_balance", "enhancement"];

static SECTIONS: &[SectionSpec] = &[
    SectionSpec {
        name: "video_adjustment",
        device_key: "videoAdjustment",
        path: "/cgi-bin/entry.cgi/image/0/video-adjustment",
        profile: false,
        fields: &[
            FieldSpec { friendly: "rotation", key: "iImageRotation", kind: FieldKind::IntEnum(&[0, 90, 180, 270]) },
            FieldSpec { friendly: "flip", key: "sImageFlip", kind: FieldKind::StrEnum(&["close", "mirror", "flip", "centrosymmetric"]) },
            FieldSpec { friendly: "power_line_frequency", key: "sPowerLineFrequencyMode", kind: FieldKind::StrEnum(&["PAL(50HZ)", "NTSC(60HZ)"]) },
        ],
        extras: &[],
    },
    SectionSpec {
        name: "night_to_day",
        device_key: "nightToDay",
        path: "/cgi-bin/entry.cgi/image/0/night-to-day",
        profile: false,
        fields: &[
            FieldSpec { friendly: "mode", key: "iMode", kind: FieldKind::IntEnum(&[0, 1, 2]) },
            FieldSpec { friendly: "filter_level", key: "iNightToDayFilterLevel", kind: FieldKind::IntEnum(&[0, 1, 2]) },
            FieldSpec { friendly: "filter_time", key: "iNightToDayFilterTime", kind: FieldKind::IntRange(1, 60) },
            FieldSpec { friendly: "dawn_time", key: "iDawnTime", kind: FieldKind::IntRange(0, 86400) },
            FieldSpec { friendly: "dusk_time", key: "iDuskTime", kind: FieldKind::IntRange(0, 86400) },
            FieldSpec { friendly: "profile_select", key: "iProfileSelect", kind: FieldKind::IntEnum(&[0, 1, 2]) },
        ],
        extras: &[("iProfileCur", "profile_current")],
    },
    SectionSpec {
        name: "adjustment",
        device_key: "imageAdjustment",
        path: "/cgi-bin/entry.cgi/image/0/{scene}/adjustment",
        profile: true,
        fields: &[
            pct("brightness", "iBrightness"),
            pct("contrast", "iContrast"),
            pct("hue", "iHue"),
            pct("saturation", "iSaturation"),
            pct("sharpness", "iSharpness"),
        ],
        extras: &[],
    },
    SectionSpec {
        name: "exposure",
        device_key: "exposure",
        path: "/cgi-bin/entry.cgi/image/0/{scene}/exposure",
        profile: true,
        fields: &[
            FieldSpec { friendly: "exposure_mode", key: "sExposureMode", kind: FieldKind::StrEnum(&["auto", "manual"]) },
            FieldSpec { friendly: "gain_mode", key: "sGainMode", kind: FieldKind::StrEnum(&["auto", "manual"]) },
            FieldSpec { friendly: "exposure_time", key: "sExposureTime", kind: FieldKind::Fraction },
            FieldSpec { friendly: "exposure_gain", key: "iExposureGain", kind: FieldKind::IntRange(0, 100) },
        ],
        extras: &[],
    },
    SectionSpec {
        name: "backlight",
        device_key: "BLC",
        path: "/cgi-bin/entry.cgi/image/0/{scene}/blc",
        profile: true,
        fields: &[
            FieldSpec { friendly: "blc_region", key: "sBLCRegion", kind: FieldKind::StrEnum(OPEN_CLOSE) },
            pct("blc_strength", "iBLCStrength"),
            pct("dark_boost_level", "iDarkBoostLevel"),
            FieldSpec { friendly: "hdr", key: "sHDR", kind: FieldKind::StrEnum(OPEN_CLOSE) },
            FieldSpec { friendly: "hdr_level", key: "iHDRLevel", kind: FieldKind::IntEnum(&[1]) },
            FieldSpec { friendly: "hlc", key: "sHLC", kind: FieldKind::StrEnum(OPEN_CLOSE) },
            FieldSpec { friendly: "hlc_level", key: "iHLCLevel", kind: FieldKind::IntRange(1, 100) },
        ],
        extras: &[],
    },
    SectionSpec {
        name: "white_balance",
        device_key: "whiteBlance",
        path: "/cgi-bin/entry.cgi/image/0/{scene}/white-blance",
        profile: true,
        fields: &[
            FieldSpec { friendly: "style", key: "sWhiteBlanceStyle", kind: FieldKind::StrEnum(&["auto", "manual", "daylight", "streetlamp", "outdoor"]) },
            FieldSpec { friendly: "color_temperature", key: "iWhiteBalanceCT", kind: FieldKind::IntRange(2800, 7500) },
        ],
        extras: &[],
    },
    SectionSpec {
        name: "enhancement",
        device_key: "imageEnhancement",
        path: "/cgi-bin/entry.cgi/image/0/{scene}/enhancement",
        profile: true,
        fields: &[
            FieldSpec { friendly: "noise_reduce_mode", key: "iNoiseReduceMode", kind: FieldKind::IntEnum(&[0, 1]) },
            pct("spatial_denoise_level", "iSpatialDenoiseLevel"),
            pct("temporal_denoise_level", "iTemporalDenoiseLevel"),
        ],
        extras: &[],
    },
];

fn find_section(name: &str) -> Result<&'static SectionSpec> {
    SECTIONS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            let names: Vec<&str> = SECTIONS.iter().map(|s| s.name).collect();
            anyhow::anyhow!("unknown section {name:?}; expected one of {names:?}")
        })
}

// MARK: Normalization (device -> friendly)

fn normalize_section(spec: &SectionSpec, raw: &Value) -> Value {
    let mut out = Map::new();
    for f in spec.fields {
        out.insert(f.friendly.to_string(), raw.get(f.key).cloned().unwrap_or(Value::Null));
    }
    for (key, friendly) in spec.extras {
        out.insert(friendly.to_string(), raw.get(key).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

pub fn normalize(config: &Value) -> Value {
    let empty = json!({});
    let profiles = config
        .get("profile")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let profiles: Vec<Value> = profiles
        .iter()
        .filter(|p| p.is_object())
        .map(|p| {
            PROFILE_SECTIONS
                .iter()
                .map(|name| {
                    let spec = find_section(name).expect("PROFILE_SECTIONS must exist in SECTIONS");
                    let raw = p.get(spec.device_key).unwrap_or(&empty);
                    (name.to_string(), normalize_section(spec, raw))
                })
                .collect::<Map<String, Value>>()
                .into()
        })
        .collect();
    json!({
        "video_adjustment": normalize_section(
            find_section("video_adjustment").expect("in SECTIONS"),
            config.get("videoAdjustment").unwrap_or(&empty),
        ),
        "night_to_day": normalize_section(
            find_section("night_to_day").expect("in SECTIONS"),
            config.get("nightToDay").unwrap_or(&empty),
        ),
        "profiles": profiles,
    })
}

pub async fn get_settings(client: &ApiClient, device: &DeviceRecord) -> Result<Value> {
    let config = client.get_json(device, PATH_IMAGE, None).await?;
    Ok(normalize(&config))
}

// MARK: Validation + read-modify-write

fn is_fraction(s: &str) -> bool {
    // Matches ^[1-9]\d*/[1-9]\d*$
    let Some((num, den)) = s.split_once('/') else {
        return false;
    };
    fn positive_int(part: &str) -> bool {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && !part.starts_with('0')
    }
    positive_int(num) && positive_int(den)
}

fn validate_value(spec: &SectionSpec, f: &FieldSpec, value: &Value) -> Result<Value> {
    let where_ = format!("{}.{}", spec.name, f.friendly);
    if value.is_boolean() {
        bail!("{where_} must not be a boolean; got {value}");
    }
    match f.kind {
        FieldKind::StrEnum(allowed) => {
            let s = value.as_str().unwrap_or("");
            if !allowed.contains(&s) {
                bail!("{where_} must be one of {allowed:?}; got {value}");
            }
        }
        FieldKind::IntEnum(allowed) => {
            let Some(i) = value.as_i64() else {
                bail!("{where_} must be one of {allowed:?}; got {value}");
            };
            if !allowed.contains(&i) {
                bail!("{where_} must be one of {allowed:?}; got {value}");
            }
        }
        FieldKind::IntRange(lo, hi) => {
            let Some(i) = value.as_i64() else {
                bail!("{where_} must be an integer; got {value}");
            };
            if i < lo || i > hi {
                bail!("{where_} must be within [{lo}, {hi}]; got {value}");
            }
        }
        FieldKind::Fraction => {
            let ok = value.as_str().map(is_fraction).unwrap_or(false);
            if !ok {
                bail!("{where_} must be a fraction string like '1/60'; got {value}");
            }
        }
    }
    Ok(value.clone())
}

fn check_section_rules(spec: &SectionSpec, merged: &Value) -> Result<()> {
    if spec.name == "backlight" {
        let open: Vec<&str> = ["sBLCRegion", "sHDR", "sHLC"]
            .into_iter()
            .filter(|k| merged.get(k).and_then(|v| v.as_str()) == Some("open"))
            .collect();
        if open.len() > 1 {
            bail!("backlight: BLC/HDR/HLC are mutually exclusive, but {open:?} would all be 'open'");
        }
    }
    if spec.name == "night_to_day" {
        let dawn = merged.get("iDawnTime").and_then(|v| v.as_i64());
        let dusk = merged.get("iDuskTime").and_then(|v| v.as_i64());
        if let (Some(dawn), Some(dusk)) = (dawn, dusk) {
            if dusk <= dawn {
                bail!("night_to_day: dusk_time ({dusk}) must be greater than dawn_time ({dawn})");
            }
        }
    }
    Ok(())
}

/// Pure core: validate `values`, merge into `config`, return (PUT path, payload).
fn build_update(
    section: &str,
    values: &Value,
    scene_id: Option<i64>,
    config: &Value,
) -> Result<(String, Value)> {
    let spec = find_section(section)?;
    if spec.profile {
        let Some(id) = scene_id else {
            bail!("section {section:?} requires scene_id in {SCENES:?}");
        };
        if !SCENES.contains(&id) {
            bail!("section {section:?} requires scene_id in {SCENES:?}");
        }
    } else if scene_id.is_some() {
        bail!("section {section:?} does not take a scene_id");
    }
    let Some(obj) = values.as_object().filter(|o| !o.is_empty()) else {
        bail!("'values' must be a non-empty object of section fields");
    };
    for key in obj.keys() {
        if !spec.fields.iter().any(|f| f.friendly == key) {
            let allowed: Vec<&str> = spec.fields.iter().map(|f| f.friendly).collect();
            bail!("unknown field(s) for section {section:?}: [{key:?}]; allowed: {allowed:?}");
        }
    }

    let mut current = Map::new();
    if spec.profile {
        let id = scene_id.expect("validated above") as usize;
        let entry = config
            .get("profile")
            .and_then(|p| p.as_array())
            .and_then(|a| a.get(id));
        match entry {
            Some(e) if e.is_object() => {
                if let Some(sec) = e.get(spec.device_key).and_then(|s| s.as_object()) {
                    current = sec.clone();
                }
            }
            _ => bail!("device returned no profile for scene_id {id}"),
        }
    } else if let Some(sec) = config.get(spec.device_key).and_then(|s| s.as_object()) {
        current = sec.clone();
    }

    for f in spec.fields {
        if let Some(v) = obj.get(f.friendly) {
            current.insert(f.key.to_string(), validate_value(spec, f, v)?);
        }
    }
    let merged = Value::Object(current);
    check_section_rules(spec, &merged)?;

    let path = if spec.profile {
        spec.path.replace("{scene}", &scene_id.expect("validated above").to_string())
    } else {
        spec.path.to_string()
    };
    Ok((path, merged))
}

pub async fn set_settings(
    client: &ApiClient,
    device: &DeviceRecord,
    section: &str,
    values: &Value,
    scene_id: Option<i64>,
) -> Result<()> {
    // Validate before any IO where possible; RMW needs the current config.
    let config = client.get_json(device, PATH_IMAGE, None).await?;
    let (path, payload) = build_update(section, values, scene_id, &config)?;
    let resp = client.put_json(device, &path, Some(&payload)).await?;
    expect_ok(&resp, &format!("set image settings {section}"))
}

// MARK: Tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Value {
        let profile = json!({
            "imageAdjustment": {"iBrightness": 50, "iContrast": 50, "iHue": 50, "iSaturation": 50, "iSharpness": 50},
            "exposure": {"iExposureGain": 1, "sExposureMode": "auto", "sExposureTime": "1/6", "sGainMode": "auto"},
            "BLC": {"sBLCRegion": "close", "iBLCStrength": 1, "iDarkBoostLevel": 50, "sHDR": "close", "iHDRLevel": 1, "sHLC": "close", "iHLCLevel": 1},
            "whiteBlance": {"iWhiteBalanceCT": 2800, "sWhiteBlanceStyle": "auto"},
            "imageEnhancement": {"iNoiseReduceMode": 1, "iSpatialDenoiseLevel": 50, "iTemporalDenoiseLevel": 50},
        });
        json!({
            "id": 0,
            "videoAdjustment": {"iImageRotation": 0, "sImageFlip": "close", "sPowerLineFrequencyMode": "NTSC(60HZ)"},
            "nightToDay": {"iMode": 0, "iNightToDayFilterLevel": 0, "iNightToDayFilterTime": 5, "iDawnTime": 28800, "iDuskTime": 64800, "iProfileSelect": 0, "iProfileCur": 1},
            "profile": [profile.clone(), profile.clone(), profile],
        })
    }

    #[test]
    fn normalize_full_config() {
        let cfg = normalize(&config());
        assert_eq!(cfg["video_adjustment"]["rotation"], 0);
        assert_eq!(cfg["night_to_day"]["dawn_time"], 28800);
        assert_eq!(cfg["night_to_day"]["profile_current"], 1);
        assert_eq!(cfg["profiles"].as_array().unwrap().len(), 3);
        assert_eq!(cfg["profiles"][0]["backlight"]["blc_region"], "close");
        assert_eq!(cfg["profiles"][0]["white_balance"]["color_temperature"], 2800);
    }

    #[test]
    fn rejects_unknown_section_and_field() {
        assert!(build_update("nope", &json!({"x": 1}), None, &config()).is_err());
        assert!(build_update("adjustment", &json!({"nope": 1}), Some(0), &config()).is_err());
    }

    #[test]
    fn scene_id_rules() {
        assert!(build_update("adjustment", &json!({"brightness": 50}), None, &config()).is_err());
        assert!(build_update("video_adjustment", &json!({"rotation": 0}), Some(1), &config()).is_err());
    }

    #[test]
    fn range_enum_fraction_and_bool_validation() {
        assert!(build_update("adjustment", &json!({"brightness": 101}), Some(0), &config()).is_err());
        assert!(build_update("video_adjustment", &json!({"rotation": 45}), None, &config()).is_err());
        assert!(build_update("exposure", &json!({"exposure_time": "6"}), Some(0), &config()).is_err());
        assert!(build_update("exposure", &json!({"exposure_time": "06/1"}), Some(0), &config()).is_err());
        assert!(build_update("exposure", &json!({"exposure_time": "1/00"}), Some(0), &config()).is_err());
        assert!(build_update("video_adjustment", &json!({"rotation": false}), None, &config()).is_err());
        assert!(build_update("backlight", &json!({"hdr_level": true}), Some(0), &config()).is_err());
    }

    #[test]
    fn read_modify_write_merges_full_section() {
        let (path, payload) =
            build_update("adjustment", &json!({"brightness": 80}), Some(2), &config()).unwrap();
        assert_eq!(path, "/cgi-bin/entry.cgi/image/0/2/adjustment");
        assert_eq!(payload["iBrightness"], 80);
        assert_eq!(payload["iContrast"], 50);
    }

    #[test]
    fn backlight_mutual_exclusion_after_merge() {
        assert!(build_update("backlight", &json!({"hdr": "open", "hlc": "open"}), Some(0), &config()).is_err());
        let (_, payload) = build_update("backlight", &json!({"hdr": "open"}), Some(0), &config()).unwrap();
        assert_eq!(payload["sHDR"], "open");
    }

    #[test]
    fn night_to_day_dusk_must_follow_dawn() {
        assert!(build_update("night_to_day", &json!({"dawn_time": 70000}), None, &config()).is_err());
        let (_, payload) = build_update("night_to_day", &json!({"dusk_time": 60000}), None, &config()).unwrap();
        assert_eq!(payload["iDuskTime"], 60000);
    }

    #[test]
    fn white_blance_uses_device_spelling() {
        let (path, payload) =
            build_update("white_balance", &json!({"color_temperature": 5000}), Some(1), &config()).unwrap();
        assert_eq!(path, "/cgi-bin/entry.cgi/image/0/1/white-blance");
        assert_eq!(payload["iWhiteBalanceCT"], 5000);
    }

    #[test]
    fn malformed_profile_entry_is_an_error() {
        let mut bad = config();
        bad["profile"].as_array_mut().unwrap()[1] = Value::Null;
        assert!(build_update("adjustment", &json!({"brightness": 50}), Some(1), &bad).is_err());
    }
}
