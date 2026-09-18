//! High-level detection facade composing `api::model`, `api::rule`, `api::daemon`,
//! and root `storage::ensure_storage`.

use anyhow::{bail, Result};

use crate::api::{daemon as api_daemon, model as api_model, rule as api_rule};
use crate::api_client::ApiClient;
use crate::storage;
use crate::types::{
    DetectionEvent, DetectionModel, DetectionRule, DeviceRecord, RecordSource, RecordTrigger,
    RuleConfig, ScheduleRange, WriterConfig,
};

// MARK: Models

pub async fn get_detection_models_info(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Vec<DetectionModel>> {
    api_model::list_models(client, device).await
}

pub async fn get_detection_model(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<DetectionModel>> {
    api_model::get_active_model(client, device).await
}

pub async fn set_detection_model(
    client: &ApiClient,
    device: &DeviceRecord,
    model_id: Option<i32>,
    model_name: Option<&str>,
) -> Result<()> {
    if model_id.is_none() == model_name.is_none() {
        bail!("Provide exactly one of 'model_id' or 'model_name'.");
    }
    let models = api_model::list_models(client, device).await?;
    let target = if let Some(id) = model_id {
        models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| anyhow::anyhow!("Model ID '{id}' not found on device."))?
    } else {
        let name = model_name.unwrap();
        models
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| anyhow::anyhow!("Model name '{name}' not found on device."))?
    };
    api_model::set_active_model(client, device, target).await
}

// MARK: Schedule (detection-level wrapper)

pub async fn get_detection_schedule(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<Vec<ScheduleRange>>> {
    api_rule::get_schedule(client, device).await
}

pub async fn set_detection_schedule(
    client: &ApiClient,
    device: &DeviceRecord,
    schedule: Option<&[ScheduleRange]>,
) -> Result<()> {
    api_rule::set_schedule(client, device, schedule).await
}

// MARK: Rules (INFERENCE_SET trigger)

pub async fn get_detection_rules(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Vec<DetectionRule>> {
    let cfg = match api_rule::get_config(client, device).await {
        Ok(c) => c,
        Err(_) => return Ok(vec![]),
    };
    if !is_record_image_enabled(&cfg) {
        return Ok(vec![]);
    }
    // Kind gate on the raw payload: a retired-but-unmigrated SED selection
    // yields [] instead of an error.
    let raw = api_rule::get_record_rule_json(client, device).await?;
    Ok(api_rule::inference_rules_from_raw(&raw))
}

/// Vision intent assumed when a rule omits `source_filter`: an empty filter
/// matches EVERY source — including acoustic classifications — which is
/// almost never what an agent compiling a vision rule wants. Use the raw
/// `set_record_trigger` for an explicit all-sources rule.
const DEFAULT_VISION_SOURCE: &str = "builtin";

pub async fn set_detection_rules(
    client: &ApiClient,
    device: &DeviceRecord,
    rules: &[DetectionRule],
) -> Result<()> {
    for (idx, rule) in rules.iter().enumerate() {
        validate_confidence_range(idx, &rule.confidence_range_filter)?;
    }
    let mut prepared: Vec<DetectionRule> = rules.to_vec();
    for rule in prepared.iter_mut() {
        if rule.source_filter.is_empty() {
            rule.source_filter = vec![DEFAULT_VISION_SOURCE.to_string()];
        }
    }
    if !prepared.is_empty() {
        let sources = api_rule::get_record_sources(client, device).await?;
        validate_rules_against_sources(&prepared, &sources)?;
    }
    ensure_record_image(client, device).await?;
    storage::ensure_storage(client, device).await?;
    let trigger = RecordTrigger::InferenceSet { rules: prepared };
    api_rule::set_trigger(client, device, &trigger).await
}

/// Compile-time check: unknown source ids and unproducible labels fail loudly
/// instead of writing a rule that can never fire. A selected source whose
/// class set is unknowable (the builtin vision source follows the selected
/// model) disables the label check — never cry wolf.
fn validate_rules_against_sources(rules: &[DetectionRule], sources: &[RecordSource]) -> Result<()> {
    for rule in rules {
        let mut picked: Vec<&RecordSource> = Vec::new();
        for sid in &rule.source_filter {
            match sources.iter().find(|s| &s.id == sid) {
                Some(s) => picked.push(s),
                None => bail!(
                    "rule {:?}: unknown source_filter id {:?}; available: {:?}",
                    rule.name,
                    sid,
                    sources.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
                ),
            }
        }
        if rule.label_filter.is_empty() {
            continue;
        }
        if picked.iter().any(|s| s.kind == "builtin") {
            continue; // builtin classes follow the active vision model: unknowable
        }
        let producible: std::collections::HashSet<&str> = picked
            .iter()
            .flat_map(|s| s.classes.iter().map(|c| c.as_str()))
            .collect();
        let bad: Vec<&str> = rule
            .label_filter
            .iter()
            .map(|l| l.as_str())
            .filter(|l| !producible.contains(l))
            .collect();
        if !bad.is_empty() {
            let states = picked
                .iter()
                .map(|s| {
                    format!(
                        "{}({}, {} classes)",
                        s.id,
                        if s.running { "running" } else { "STOPPED" },
                        s.classes.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "rule {:?}: label(s) {:?} cannot be produced by the selected source(s) [{}]; \
                 check get_record_sources and the AcousticsLab/App Center state first",
                rule.name,
                bad,
                states
            );
        }
    }
    Ok(())
}

fn validate_confidence_range(idx: usize, range: &[f64]) -> Result<()> {
    if range.len() != 2 {
        bail!(
            "rule #{idx}: confidence_range_filter must be exactly [min, max]; got {} value(s)",
            range.len()
        );
    }
    let (min, max) = (range[0], range[1]);
    if !(0.0..=1.0).contains(&min) || !(0.0..=1.0).contains(&max) {
        bail!(
            "rule #{idx}: confidence_range_filter values must be within [0.0, 1.0]; got [{min}, {max}]"
        );
    }
    if min > max {
        bail!("rule #{idx}: confidence_range_filter min ({min}) must be <= max ({max})");
    }
    Ok(())
}

// MARK: Events (daemon)

pub async fn get_detection_events(
    client: &ApiClient,
    device: &DeviceRecord,
    start_unix_ms: Option<i64>,
    end_unix_ms: Option<i64>,
) -> Result<Vec<DetectionEvent>> {
    api_daemon::get_events(client, device, start_unix_ms, end_unix_ms).await
}

pub async fn clear_detection_events(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    api_daemon::clear_events(client, device).await
}

// MARK: Helpers

fn is_record_image_enabled(cfg: &RuleConfig) -> bool {
    cfg.rule_enabled && cfg.writer.format.eq_ignore_ascii_case("JPG")
}

async fn ensure_record_image(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let cfg = api_rule::get_config(client, device).await.ok();
    if cfg.as_ref().is_some_and(is_record_image_enabled) {
        return Ok(());
    }
    let new_cfg = RuleConfig {
        rule_enabled: true,
        writer: WriterConfig {
            format: "JPG".into(),
            interval_ms: 0,
        },
    };
    api_rule::set_config(client, device, &new_cfg).await
}

#[cfg(test)]
mod tests {
    use super::validate_confidence_range;

    #[test]
    fn validate_confidence_range_accepts_valid() {
        assert!(validate_confidence_range(0, &[0.0, 1.0]).is_ok());
        assert!(validate_confidence_range(0, &[0.25, 0.75]).is_ok());
        assert!(validate_confidence_range(0, &[0.5, 0.5]).is_ok());
    }

    #[test]
    fn validate_confidence_range_rejects_bad_length() {
        assert!(validate_confidence_range(0, &[]).is_err());
        assert!(validate_confidence_range(0, &[0.5]).is_err());
        assert!(validate_confidence_range(0, &[0.0, 0.5, 1.0]).is_err());
    }

    #[test]
    fn validate_confidence_range_rejects_out_of_range() {
        assert!(validate_confidence_range(0, &[-0.01, 0.5]).is_err());
        assert!(validate_confidence_range(0, &[0.0, 1.01]).is_err());
    }

    #[test]
    fn validate_confidence_range_rejects_inverted() {
        assert!(validate_confidence_range(0, &[0.8, 0.2]).is_err());
    }
}
