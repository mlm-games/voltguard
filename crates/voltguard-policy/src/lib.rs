use serde::{Deserialize, Serialize};
use std::sync::Arc;
use voltguard_api::*;
use voltguard_core::PowerManager;

// Policy Engine

pub struct PolicyEngine {
    power_manager: Arc<PowerManager>,
    policies: tokio::sync::RwLock<Vec<Box<dyn Policy>>>,
}

impl PolicyEngine {
    pub fn new(power_manager: Arc<PowerManager>) -> Self {
        Self {
            power_manager,
            policies: tokio::sync::RwLock::new(Vec::new()),
        }
    }

    pub async fn add_policy(&self, policy: Box<dyn Policy>) {
        self.policies.write().await.push(policy);
    }

    pub async fn evaluate_all(&self) -> Vec<PolicyViolation> {
        let policies = self.policies.read().await;
        let mut violations = Vec::new();

        for policy in policies.iter() {
            if let Some(violation) = policy.evaluate(&*self.power_manager).await {
                violations.push(violation);
            }
        }

        violations
    }

    pub async fn enforce(&self) -> Result<()> {
        let violations = self.evaluate_all().await;

        for violation in violations {
            if let Some(action) = violation.remediation {
                self.apply_action(action).await?;
            }
        }

        Ok(())
    }

    async fn apply_action(&self, action: PolicyAction) -> Result<()> {
        match action {
            PolicyAction::SetProfile(profile) => {
                self.power_manager.set_profile(profile).await?;
            }
            PolicyAction::ApplyCapability { component_id, capability } => {
                self.power_manager.apply_capability(component_id, capability).await?;
            }
            PolicyAction::Notify(message) => {
                tracing::warn!("Policy notification: {}", message);
            }
        }
        Ok(())
    }
}

// Policy Trait and Types

#[async_trait::async_trait]
pub trait Policy: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn evaluate(&self, manager: &PowerManager) -> Option<PolicyViolation>;
}

#[derive(Debug, Clone)]
pub struct PolicyViolation {
    pub policy_name: String,
    pub severity: Severity,
    pub message: String,
    pub remediation: Option<PolicyAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone)]
pub enum PolicyAction {
    SetProfile(PowerProfile),
    ApplyCapability { component_id: ComponentId, capability: Capability },
    Notify(String),
}

// Built-in Policies (placeholders)

/// Battery threshold policy - switch to power save when battery low
pub struct BatteryThresholdPolicy {
    threshold: f32,
}

impl BatteryThresholdPolicy {
    pub fn new(threshold: f32) -> Self {
        Self { threshold }
    }
}

#[async_trait::async_trait]
impl Policy for BatteryThresholdPolicy {
    fn name(&self) -> &str {
        "battery_threshold"
    }

    fn description(&self) -> &str {
        "Switch to power save mode when battery is low"
    }

    async fn evaluate(&self, _manager: &PowerManager) -> Option<PolicyViolation> {
        // Placeholder: no evaluation yet
        let _ = self.threshold;
        None
    }
}

/// Thermal policy - reduce performance when temperature is high
pub struct ThermalPolicy {
    max_temperature: f32,
}

#[async_trait::async_trait]
impl Policy for ThermalPolicy {
    fn name(&self) -> &str {
        "thermal_protection"
    }

    fn description(&self) -> &str {
        "Reduce performance when system temperature is too high"
    }

    async fn evaluate(&self, _manager: &PowerManager) -> Option<PolicyViolation> {
        let _ = self.max_temperature;
        None
    }
}

/// Time-based policy - automatically switch profiles based on time of day
#[derive(Serialize, Deserialize)]
pub struct TimeBasedPolicy {
    schedules: Vec<Schedule>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Schedule {
    pub start_time: chrono::NaiveTime,
    pub end_time: chrono::NaiveTime,
    pub profile: PowerProfile,
}

#[async_trait::async_trait]
impl Policy for TimeBasedPolicy {
    fn name(&self) -> &str {
        "time_based"
    }

    fn description(&self) -> &str {
        "Switch power profiles based on time of day"
    }

    async fn evaluate(&self, manager: &PowerManager) -> Option<PolicyViolation> {
        let now = chrono::Local::now().time();
        let current_profile = manager.current_profile().await;

        for schedule in &self.schedules {
            if now >= schedule.start_time && now < schedule.end_time {
                if current_profile != schedule.profile {
                    return Some(PolicyViolation {
                        policy_name: self.name().to_string(),
                        severity: Severity::Info,
                        message: format!("Time-based profile change to {:?}", schedule.profile),
                        remediation: Some(PolicyAction::SetProfile(schedule.profile)),
                    });
                }
                break;
            }
        }

        None
    }
}
