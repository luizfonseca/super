use std::{borrow::Cow, collections::HashMap};

use anyhow::{anyhow, Result};

/// Get a required configuration value from a plugin config
pub(crate) fn get_required_config(
    plugin_config: &HashMap<Cow<'static, str>, serde_json::Value>,
    key: &str,
) -> Result<String> {
    plugin_config
        .get(key)
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("Missing or invalid {key}"))
}
