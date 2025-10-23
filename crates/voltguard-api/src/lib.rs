use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// Core Domain Types

/// Unique identifier for hardware components
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentId(pub Uuid);

impl ComponentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Power measurement with uncertainty bounds
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PowerMeasurement {
    pub watts: f64,
    pub uncertainty: f64,
    pub timestamp: DateTime<Utc>,
}

/// Hardware component types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ComponentType {
    Cpu,
    Gpu,
    Disk,
    Network,
    Display,
    Usb,
    Audio,
    Memory,
    Pcie,
    Custom,
}

/// Component state representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentState {
    pub id: ComponentId,
    pub component_type: ComponentType,
    pub name: String,
    pub power: Option<PowerMeasurement>,
    pub properties: ComponentProperties,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentProperties {
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub driver: Option<String>,
    pub metadata: std::collections::HashMap<String, String>,
}

/// Capabilities that components can support
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    // CPU
    FrequencyScaling {
        min: u64,
        max: u64,
        current: u64,
    },
    GovernorControl {
        available: Vec<String>,
        current: String,
    },
    CoreControl {
        total: u32,
        online: u32,
    },
    TurboBoost {
        enabled: bool,
    },

    // GPU
    PowerLimit {
        min: u64,
        max: u64,
        current: u64,
    },
    PerformanceLevel {
        levels: Vec<String>,
        current: String,
    },

    // Disk
    Apm {
        level: u8,
    },
    WriteCache {
        enabled: bool,
    },

    // Network
    WakeOnLan {
        enabled: bool,
    },
    PowerSave {
        enabled: bool,
    },

    // Display
    Brightness {
        min: u8,
        max: u8,
        current: u8,
    },
    AutoBrightness {
        enabled: bool,
    },

    // Generic
    RuntimePowerManagement {
        enabled: bool,
        autosuspend_delay: Option<Duration>,
    },
    Custom {
        name: String,
        value: serde_json::Value,
    },
}

// Event System

/// Events emitted by the system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    ComponentDiscovered(ComponentState),
    ComponentRemoved(ComponentId),
    ComponentStateChanged {
        id: ComponentId,
        capability: Capability,
    },
    PowerMeasurement {
        id: ComponentId,
        measurement: PowerMeasurement,
    },
    PowerProfileChanged {
        from: PowerProfile,
        to: PowerProfile,
    },
    BatteryStateChanged(BatteryState),
    ThermalEvent(ThermalEvent),
    PolicyViolation {
        policy: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerProfile {
    Performance,
    Balanced,
    PowerSave,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryState {
    pub present: bool,
    pub charging: bool,
    pub percentage: Option<f32>,
    pub time_to_empty: Option<Duration>,
    pub time_to_full: Option<Duration>,
    pub health: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalEvent {
    pub zone: String,
    pub temperature: f32,
    pub threshold: Option<f32>,
    pub severity: ThermalSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThermalSeverity {
    Normal,
    Warning,
    Critical,
}

// Results and Errors

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Component not found: {0:?}")]
    ComponentNotFound(ComponentId),

    #[error("Capability not supported: {0}")]
    CapabilityNotSupported(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Hardware error: {0}")]
    HardwareError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// IPC Protocol (versioned)

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

pub const PROTOCOL: ProtocolVersion = ProtocolVersion { major: 1, minor: 1 };

#[derive(Debug, Serialize, Deserialize)]
pub enum IpcRequest {
    Hello(ProtocolVersion),
    GetProfile,
    SetProfile(PowerProfile),
    GetComponents,
    ApplyCapability {
        id: ComponentId,
        capability: Capability,
    },
    GetTotalPower,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IpcResponse {
    HelloAck(ProtocolVersion),
    Success,
    Error(String),
    Profile(PowerProfile),
    Components(Vec<ComponentState>),
    TotalPower(f64),
}
