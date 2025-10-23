use futures_util::StreamExt;
use std::sync::Arc;
use tokio::time::interval;
use voltguard_api::*;
use voltguard_core::PowerManager;
use voltguard_hal::PowerMeter;

pub struct MonitoringEngine {
    power_manager: Arc<PowerManager>,
    metrics_collector: Arc<MetricsCollector>,
    meters: Vec<Arc<dyn PowerMeter>>,
}

impl MonitoringEngine {
    pub fn new(power_manager: Arc<PowerManager>, meters: Vec<Arc<dyn PowerMeter>>) -> Self {
        Self {
            power_manager,
            metrics_collector: Arc::new(MetricsCollector::new()),
            meters,
        }
    }

    pub async fn start(&self) -> Result<()> {
        // Monitor power meters
        for meter in &self.meters {
            let meter = meter.clone();
            let collector = self.metrics_collector.clone();
            tokio::spawn(async move {
                if let Ok(points) = meter.measurement_points().await {
                    for _p in points {
                        // share one stream per meter (package-level)
                        if let Ok(mut stream) = meter
                            .start_monitoring(std::time::Duration::from_secs(1))
                            .await
                        {
                            while let Some(m) = stream.next().await {
                                // Use a synthetic component ID per meter point if needed; for now,
                                // aggregate total power only
                                // Record under a stable synthetic ID derived from meter address/name could be added later
                                // For MVP, just push one entry into a global bucket:
                                collector.record_power(ComponentId::new(), m).await;
                            }
                        }
                    }
                }
            });
        }

        Ok(())
    }

    pub fn metrics(&self) -> Arc<MetricsCollector> {
        self.metrics_collector.clone()
    }
}

// Metrics Collector

use std::collections::{HashMap, VecDeque};
use tokio::sync::RwLock;

pub struct MetricsCollector {
    power_history: RwLock<HashMap<ComponentId, VecDeque<PowerMeasurement>>>,
    max_history: usize,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            power_history: RwLock::new(HashMap::new()),
            max_history: 3600, // 1 hour at 1 sample/sec
        }
    }

    pub async fn record_power(&self, id: ComponentId, measurement: PowerMeasurement) {
        let mut history = self.power_history.write().await;
        let entry = history.entry(id).or_insert_with(VecDeque::new);

        entry.push_back(measurement);

        if entry.len() > self.max_history {
            entry.pop_front();
        }
    }

    pub async fn get_average_power(
        &self,
        id: ComponentId,
        duration: std::time::Duration,
    ) -> Option<f64> {
        let history = self.power_history.read().await;
        let measurements = history.get(&id)?;

        if measurements.is_empty() {
            return None;
        }

        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(duration).ok()?;

        let recent: Vec<_> = measurements
            .iter()
            .filter(|m| m.timestamp > cutoff)
            .collect();

        if recent.is_empty() {
            return None;
        }

        let sum: f64 = recent.iter().map(|m| m.watts).sum();
        Some(sum / recent.len() as f64)
    }

    pub async fn get_total_power(&self) -> f64 {
        let history = self.power_history.read().await;

        history
            .keys()
            .filter_map(|id| {
                let measurements = history.get(id)?;
                measurements.back().map(|m| m.watts)
            })
            .sum()
    }
}

// Optimization Engine

pub struct OptimizationEngine {
    power_manager: Arc<PowerManager>,
    metrics: Arc<MetricsCollector>,
}

impl OptimizationEngine {
    pub fn new(power_manager: Arc<PowerManager>, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            power_manager,
            metrics,
        }
    }

    /// Run optimization analysis
    pub async fn analyze_and_suggest(&self) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();
        let components = self.power_manager.get_components().await;

        for component in components {
            suggestions.extend(self.analyze_component(&component).await);
        }

        suggestions
    }

    async fn analyze_component(&self, component: &ComponentState) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();

        if let Some(avg_power) = self
            .metrics
            .get_average_power(component.id, std::time::Duration::from_secs(300))
            .await
        {
            if avg_power > self.get_threshold(component.component_type) {
                suggestions.push(Suggestion {
                    component_id: component.id,
                    severity: SuggestionSeverity::Medium,
                    description: format!(
                        "{} consuming {:.2}W, consider optimization",
                        component.name, avg_power
                    ),
                    action: Some(SuggestionAction::ApplyCapability(
                        self.get_optimization_capability(component),
                    )),
                });
            }
        }

        suggestions
    }

    fn get_threshold(&self, component_type: ComponentType) -> f64 {
        match component_type {
            ComponentType::Cpu => 15.0,
            ComponentType::Gpu => 20.0,
            _ => 5.0,
        }
    }

    fn get_optimization_capability(&self, component: &ComponentState) -> Capability {
        match component.component_type {
            ComponentType::Cpu => {
                for cap in &component.capabilities {
                    if let Capability::GovernorControl { available, .. } = cap {
                        if available.contains(&"powersave".to_string()) {
                            return Capability::GovernorControl {
                                available: available.clone(),
                                current: "powersave".to_string(),
                            };
                        }
                    }
                }
            }
            _ => {}
        }

        Capability::RuntimePowerManagement {
            enabled: true,
            autosuspend_delay: Some(std::time::Duration::from_secs(2)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub component_id: ComponentId,
    pub severity: SuggestionSeverity,
    pub description: String,
    pub action: Option<SuggestionAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone)]
pub enum SuggestionAction {
    ApplyCapability(Capability),
    ChangeProfile(PowerProfile),
    Custom(String),
}
