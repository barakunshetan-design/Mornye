//! Argument readers and reply builders shared by the method tables.

use super::{Args, Reply};
use serde_json::Value;
use std::fmt::Display;

pub(super) fn required<'a>(args: &'a Args, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing {key}"))
}

pub(super) fn string(args: &Args, key: &str, default: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

pub(super) fn flag(args: &Args, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(super) fn ids(raw: &str) -> Result<Vec<String>, String> {
    serde_json::from_str(raw).map_err(error)
}

/// `setProviderPriority` and `getProviderPriority` address the download list.
pub(super) fn priority_kind(method: &str) -> &'static str {
    if method.contains("Metadata") {
        "metadata"
    } else {
        "download"
    }
}

/// An extension action reply must be a JSON object with `"success": true`;
/// otherwise its `error` or `message` becomes the failure (as on Android).
pub(super) fn require_successful_action(
    id: &str,
    action: &str,
    response: &str,
) -> Result<(), String> {
    let preview = || response.chars().take(240).collect::<String>();
    let reply: Value = serde_json::from_str(response).map_err(|_| {
        format!(
            "Extension {action} for {id} returned invalid JSON: {}",
            preview()
        )
    })?;
    if reply.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let detail = ["error", "message"]
        .iter()
        .filter_map(|key| reply.get(*key).and_then(Value::as_str))
        .find(|text| !text.trim().is_empty())
        .map_or_else(preview, str::to_owned);
    Err(format!("Extension {action} failed for {id}: {detail}"))
}

pub(super) fn text<E: Display>(result: Result<String, E>) -> Reply {
    result.map(Value::String).map_err(error)
}

pub(super) fn unit<E: Display>(result: Result<(), E>) -> Reply {
    result.map(|()| Value::Null).map_err(error)
}

pub(super) fn error(error: impl Display) -> String {
    error.to_string()
}
