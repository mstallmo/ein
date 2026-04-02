// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use std::collections::HashMap;

/// Per-plugin configuration stored in `~/.ein/config.json`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PluginConfig {
    /// Plugin-specific filesystem paths, unioned with the global allowed_paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_paths: Vec<String>,
    /// Plugin-specific network hosts, unioned with the global allowed_hosts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// Plugin-specific parameters forwarded as JSON (e.g. api_key, base_url, model).
    /// Values use native JSON types — numbers, strings, booleans.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, serde_json::Value>,
}

/// Client-side session config loaded from `~/.ein/config.json`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Per-plugin configuration keyed by plugin name (e.g. "ein_openrouter", "Bash").
    #[serde(default)]
    pub plugin_configs: HashMap<String, PluginConfig>,
    /// Name of the model client plugin to use (e.g. "ein_openrouter", "ein_ollama").
    /// If empty, the server picks the first available plugin.
    #[serde(default)]
    pub model_client_name: String,
}

/// Loads `~/.ein/config.json`, creating it with defaults if absent.
/// Migrates legacy flat config (api_key, base_url, model, max_tokens at root)
/// into plugin_configs["ein_openrouter"].params automatically.
pub fn load_or_create_config() -> anyhow::Result<ClientConfig> {
    let config_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".ein")
        .join("config.json");

    if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)?;
        let mut value: serde_json::Value = serde_json::from_str(&raw)?;
        if migrate_v1_to_v2(&mut value) {
            std::fs::write(&config_path, serde_json::to_string_pretty(&value)?)?;
        }
        Ok(serde_json::from_value(value)?)
    } else {
        let default = ClientConfig::default();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&config_path, serde_json::to_string_pretty(&default)?)?;
        Ok(default)
    }
}

/// Returns true if the value was modified (migration performed).
fn migrate_v1_to_v2(value: &mut serde_json::Value) -> bool {
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => return false,
    };

    let legacy_keys = ["api_key", "base_url", "model", "max_tokens"];
    let has_legacy = legacy_keys.iter().any(|k| obj.contains_key(*k));
    if !has_legacy {
        return false;
    }

    let mut plugin_cfg: HashMap<String, serde_json::Value> = HashMap::new();
    for key in &legacy_keys {
        if let Some(v) = obj.remove(*key) {
            plugin_cfg.insert(key.to_string(), v);
        }
    }

    // Merge into existing plugin_configs["ein_openrouter"].params or create it.
    let plugin_configs = obj
        .entry("plugin_configs")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    if let Some(plugins) = plugin_configs.as_object_mut() {
        let openrouter = plugins
            .entry("ein_openrouter")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

        if let Some(or_obj) = openrouter.as_object_mut() {
            let params = or_obj
                .entry("params")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

            if let Some(cfg_obj) = params.as_object_mut() {
                for (k, v) in plugin_cfg {
                    cfg_obj.entry(k).or_insert(v);
                }
            }
        }
    }

    true
}
