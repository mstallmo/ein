/// Client-side session config loaded from `~/.ein/config.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default = "ClientConfig::default_model")]
    pub model: String,
    #[serde(default = "ClientConfig::default_max_tokens")]
    pub max_tokens: i32,
}

impl ClientConfig {
    pub fn default_model() -> String {
        "anthropic/claude-haiku-4.5".to_string()
    }
    pub fn default_max_tokens() -> i32 {
        2500
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            allowed_paths: vec![],
            allowed_hosts: vec![],
            model: Self::default_model(),
            max_tokens: Self::default_max_tokens(),
        }
    }
}

/// Loads `~/.ein/config.json`, creating it with defaults if absent.
pub fn load_or_create_config() -> anyhow::Result<ClientConfig> {
    let config_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".ein")
        .join("config.json");

    if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)?;
        Ok(serde_json::from_str(&raw)?)
    } else {
        let default = ClientConfig::default();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&config_path, serde_json::to_string_pretty(&default)?)?;
        Ok(default)
    }
}
