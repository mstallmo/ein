// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use std::collections::HashMap;
use std::fs;

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
        let raw = fs::read_to_string(&config_path)?;
        let mut value: serde_json::Value = serde_json::from_str(&raw)?;

        if migrate_v1_to_v2(&mut value) {
            fs::write(&config_path, serde_json::to_string_pretty(&value)?)?;
        }

        Ok(serde_json::from_value(value)?)
    } else {
        let default = ClientConfig::default();

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&config_path, serde_json::to_string_pretty(&default)?)?;

        Ok(default)
    }
}

/// Returns true when no model provider has been configured (first-run state).
pub fn is_first_run(cfg: &ClientConfig) -> bool {
    cfg.model_client_name.is_empty() && cfg.plugin_configs.is_empty()
}

/// Writes `cfg` to `~/.ein/config.json`, creating parent directories as needed.
pub fn save_config(cfg: &ClientConfig) -> anyhow::Result<()> {
    let config_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".ein")
        .join("config.json");

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&config_path, serde_json::to_string_pretty(cfg)?)?;

    Ok(())
}

/// Constructs a `ClientConfig` from wizard-collected inputs for a given provider.
///
/// Provider-specific defaults are applied when optional fields are left blank:
/// - `ein_openrouter`: `base_url` defaults to `https://openrouter.ai/api/v1`
/// - `ein_ollama`: `base_url` defaults to `http://localhost:11434`
pub fn build_config_for_provider(
    provider: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> ClientConfig {
    let mut params: HashMap<String, serde_json::Value> = HashMap::new();

    match provider {
        "ein_openrouter" => {
            params.insert("api_key".to_string(), serde_json::json!(api_key));

            let url = if base_url.is_empty() {
                "https://openrouter.ai/api/v1"
            } else {
                base_url
            };
            params.insert("base_url".to_string(), serde_json::json!(url));

            if !model.is_empty() {
                params.insert("model".to_string(), serde_json::json!(model));
            }
        }
        "ein_anthropic" => {
            params.insert("api_key".to_string(), serde_json::json!(api_key));

            if !model.is_empty() {
                params.insert("model".to_string(), serde_json::json!(model));
            }
        }
        "ein_openai" => {
            params.insert("api_key".to_string(), serde_json::json!(api_key));

            if !base_url.is_empty() {
                params.insert("base_url".to_string(), serde_json::json!(base_url));
            }

            if !model.is_empty() {
                params.insert("model".to_string(), serde_json::json!(model));
            }
        }
        "ein_ollama" => {
            let url = if base_url.is_empty() {
                "http://localhost:11434"
            } else {
                base_url
            };
            params.insert("base_url".to_string(), serde_json::json!(url));

            if !model.is_empty() {
                params.insert("model".to_string(), serde_json::json!(model));
            }
        }
        _ => {}
    }

    let mut plugin_configs = HashMap::new();
    plugin_configs.insert(
        provider.to_string(),
        PluginConfig {
            allowed_paths: vec![],
            allowed_hosts: vec![],
            params,
        },
    );

    ClientConfig {
        model_client_name: provider.to_string(),
        plugin_configs,
        ..Default::default()
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_first_run_empty_config() {
        assert!(is_first_run(&ClientConfig::default()));
    }

    #[test]
    fn is_first_run_false_when_model_client_name_set() {
        let cfg = ClientConfig {
            model_client_name: "ein_openrouter".to_string(),
            ..Default::default()
        };
        assert!(!is_first_run(&cfg));
    }

    #[test]
    fn is_first_run_false_when_plugin_configs_set() {
        let cfg = build_config_for_provider("ein_openrouter", "key", "", "model");
        assert!(!is_first_run(&cfg));
    }

    #[test]
    fn build_config_openrouter_default_base_url() {
        let cfg = build_config_for_provider("ein_openrouter", "sk-key", "", "my-model");
        let params = &cfg.plugin_configs["ein_openrouter"].params;

        assert_eq!(
            params["base_url"].as_str().unwrap(),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(params["api_key"].as_str().unwrap(), "sk-key");
        assert_eq!(params["model"].as_str().unwrap(), "my-model");
        assert_eq!(cfg.model_client_name, "ein_openrouter");
    }

    #[test]
    fn build_config_openrouter_custom_base_url() {
        let cfg =
            build_config_for_provider("ein_openrouter", "key", "https://custom.example.com", "m");

        assert_eq!(
            cfg.plugin_configs["ein_openrouter"].params["base_url"]
                .as_str()
                .unwrap(),
            "https://custom.example.com"
        );
    }

    #[test]
    fn build_config_anthropic_no_base_url() {
        let cfg = build_config_for_provider("ein_anthropic", "sk-ant", "", "claude-opus-4-7");
        let params = &cfg.plugin_configs["ein_anthropic"].params;

        assert!(
            !params.contains_key("base_url"),
            "Anthropic should not have base_url"
        );
        assert_eq!(params["api_key"].as_str().unwrap(), "sk-ant");
        assert_eq!(cfg.model_client_name, "ein_anthropic");
    }

    #[test]
    fn build_config_ollama_no_api_key_default_url() {
        let cfg = build_config_for_provider("ein_ollama", "", "", "llama3");
        let params = &cfg.plugin_configs["ein_ollama"].params;

        assert!(
            !params.contains_key("api_key"),
            "Ollama should not have api_key"
        );
        assert_eq!(
            params["base_url"].as_str().unwrap(),
            "http://localhost:11434"
        );
    }

    #[test]
    fn save_config_roundtrip() {
        let cfg = build_config_for_provider("ein_openrouter", "test-key", "", "test-model");
        let dir = std::env::temp_dir().join(format!("ein_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let path = dir.join("config.json");
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        fs::write(&path, &json).unwrap();

        let loaded: ClientConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.model_client_name, "ein_openrouter");
        assert_eq!(
            loaded.plugin_configs["ein_openrouter"].params["api_key"]
                .as_str()
                .unwrap(),
            "test-key"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
