use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use voltguard_api::*;
use voltguard_hal::*;

// Power Manager (Core Orchestrator)

pub struct PowerManager {
    registry: Arc<DriverRegistry>,
    components: RwLock<HashMap<ComponentId, ComponentState>>,
    event_tx: broadcast::Sender<SystemEvent>,
    current_profile: RwLock<PowerProfile>,
}

impl PowerManager {
    pub fn new(registry: Arc<DriverRegistry>) -> Self {
        let (event_tx, _) = broadcast::channel(1000);

        Self {
            registry,
            components: RwLock::new(HashMap::new()),
            event_tx,
            current_profile: RwLock::new(PowerProfile::Balanced),
        }
    }

    /// Initialize the power manager
    pub async fn initialize(&self) -> Result<()> {
        // Discover all components
        let discovered = self.registry.discover_all().await?;

        let mut components = self.components.write().await;
        for component in discovered {
            let id = component.id;
            components.insert(id, component.clone());
            let _ = self
                .event_tx
                .send(SystemEvent::ComponentDiscovered(component));
        }

        tracing::info!("Discovered {} components", components.len());
        Ok(())
    }

    /// Get current power profile
    pub async fn current_profile(&self) -> PowerProfile {
        *self.current_profile.read().await
    }

    /// Switch power profile
    pub async fn set_profile(&self, profile: PowerProfile) -> Result<()> {
        let old_profile = *self.current_profile.read().await;
        *self.current_profile.write().await = profile;

        // Apply profile-specific optimizations
        self.apply_profile_optimizations(profile).await?;

        let _ = self.event_tx.send(SystemEvent::PowerProfileChanged {
            from: old_profile,
            to: profile,
        });

        Ok(())
    }

    /// Apply a capability to a component
    pub async fn apply_capability(&self, id: ComponentId, capability: Capability) -> Result<()> {
        let components = self.components.read().await;
        let component = components.get(&id).ok_or(Error::ComponentNotFound(id))?;

        let driver = self
            .registry
            .get_driver(component.component_type)
            .await
            .ok_or_else(|| Error::Other(anyhow::anyhow!("No driver for component type")))?;

        // Validate before applying
        if !driver.validate_capability(id, &capability).await? {
            return Err(Error::CapabilityNotSupported(format!("{:?}", capability)));
        }

        driver.apply_capability(id, capability.clone()).await?;

        let _ = self
            .event_tx
            .send(SystemEvent::ComponentStateChanged { id, capability });

        Ok(())
    }

    /// Subscribe to system events
    pub fn subscribe_events(&self) -> broadcast::Receiver<SystemEvent> {
        self.event_tx.subscribe()
    }

    /// Get all components
    pub async fn get_components(&self) -> Vec<ComponentState> {
        self.components.read().await.values().cloned().collect()
    }

    /// Get component by ID
    pub async fn get_component(&self, id: ComponentId) -> Option<ComponentState> {
        self.components.read().await.get(&id).cloned()
    }

    async fn apply_profile_optimizations(&self, profile: PowerProfile) -> Result<()> {
        // Apply profile-specific settings to all components
        let components = self.get_components().await;

        for component in components {
            match profile {
                PowerProfile::PowerSave => {
                    self.apply_powersave_settings(&component).await?;
                }
                PowerProfile::Performance => {
                    self.apply_performance_settings(&component).await?;
                }
                PowerProfile::Balanced => {
                    self.apply_balanced_settings(&component).await?;
                }
                PowerProfile::Custom => {}
            }
        }

        Ok(())
    }

    async fn apply_powersave_settings(&self, component: &ComponentState) -> Result<()> {
        match component.component_type {
            ComponentType::Cpu => {
                for cap in &component.capabilities {
                    if let Capability::GovernorControl { available, .. } = cap {
                        if available.iter().any(|g| g == "powersave") {
                            self.apply_capability(
                                component.id,
                                Capability::GovernorControl {
                                    available: available.clone(),
                                    current: "powersave".to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn apply_performance_settings(&self, _component: &ComponentState) -> Result<()> {
        Ok(())
    }

    async fn apply_balanced_settings(&self, _component: &ComponentState) -> Result<()> {
        Ok(())
    }
}
