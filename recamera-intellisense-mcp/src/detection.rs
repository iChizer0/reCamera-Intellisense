//! High-level detection facade composing `api::model`, `api::rule`, `api::daemon`,
//! and root `storage::ensure_storage`.

use anyhow::{bail, Result};

use crate::api::{daemon as api_daemon, model as api_model, rule as api_rule};
use crate::api_client::ApiClient;
use crate::storage;
use crate::types::{
    DetectionEvent, DetectionModel, DetectionRule, DeviceRecord, RecordTrigger, RuleConfig,
    ScheduleRange, WriterConfig,
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
    match api_rule::get_trigger(client, device).await? {
        RecordTrigger::InferenceSet { rules } => Ok(rules),
        _ => Ok(vec![]),
    }
}

pub async fn set_detection_rules(
    client: &ApiClient,
    device: &DeviceRecord,
    rules: &[DetectionRule],
) -> Result<()> {
    ensure_record_image(client, device).await?;
    storage::ensure_storage(client, device).await?;
    let trigger = RecordTrigger::InferenceSet {
        rules: rules.to_vec(),
    };
    api_rule::set_trigger(client, device, &trigger).await
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
