use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, time::Duration};

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub profiles: ProfilesConfig,
    pub policies: PoliciesConfig,
    pub monitoring: MonitoringConfig,
    pub schema_version: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: Default::default(),
            profiles: Default::default(),
            policies: Default::default(),
            monitoring: Default::default(),
            schema_version: 1,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub log_level: String,
    pub pid_file: PathBuf,
}
impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: "/var/run/voltguard.sock".into(),
            log_level: "info".into(),
            pid_file: "/run/voltguard.pid".into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Default)]
#[serde(default)]
pub struct ProfilesConfig {
    pub default: String,
    pub custom_profiles: HashMap<String, CustomProfile>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Default)]
#[serde(default)]
pub struct CustomProfile {
    pub cpu_governor: Option<String>,
    pub gpu_power_limit: Option<u64>,
    pub disk_apm_level: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Default)]
#[serde(default)]
pub struct PoliciesConfig {
    pub battery_threshold: Option<f32>,
    pub thermal_limit: Option<f32>,
    pub time_based: Vec<TimeBasedRule>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct TimeBasedRule {
    pub start: String,
    pub end: String,
    pub profile: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
#[serde(default)]
pub struct MonitoringConfig {
    pub interval: Duration,
    pub history_size: usize,
    pub export_metrics: bool,
}
impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            history_size: 3600,
            export_metrics: true,
        }
    }
}

pub fn load() -> anyhow::Result<Config> {
    use figment::{
        providers::{Env, Format, Serialized, Toml},
        Figment,
    };
    let system = "/etc/voltguard/voltguard.toml";
    let user = dirs::config_dir()
        .unwrap_or_default()
        .join("voltguard/voltguard.toml");

    let figment = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(system))
        .merge(Toml::file(user))
        .merge(Env::prefixed("VOLTGUARD_").split("__"));
    Ok(figment.extract()?)
}
