use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
use voltguard_api::*;

// Core HAL Traits

/// Trait for hardware component discovery and management
#[async_trait]
pub trait ComponentDriver: Send + Sync {
    /// Get the type of components this driver handles
    fn component_type(&self) -> ComponentType;

    /// Discover all components of this type
    async fn discover(&self) -> Result<Vec<ComponentState>>;

    /// Read current state of a component
    async fn read_state(&self, id: ComponentId) -> Result<ComponentState>;

    /// Apply a capability change to a component
    async fn apply_capability(&self, id: ComponentId, capability: Capability) -> Result<()>;

    /// Monitor component for changes (returns stream of events)
    async fn monitor(&self, id: ComponentId) -> Result<ComponentStream>;

    /// Validate if a capability can be applied
    async fn validate_capability(&self, id: ComponentId, capability: &Capability) -> Result<bool>;
}

pub type ComponentStream = std::pin::Pin<Box<dyn futures::Stream<Item = SystemEvent> + Send>>;

/// Trait for power measurement backends
#[async_trait]
pub trait PowerMeter: Send + Sync {
    /// Get supported measurement points
    async fn measurement_points(&self) -> Result<Vec<MeasurementPoint>>;

    /// Read power consumption
    async fn read_power(&self, point: &MeasurementPoint) -> Result<PowerMeasurement>;

    /// Start continuous monitoring
    async fn start_monitoring(
        &self,
        point: MeasurementPoint,
        interval: std::time::Duration,
    ) -> Result<MeasurementStream>;
}

#[derive(Debug, Clone)]
pub struct MeasurementPoint {
    pub id: ComponentId,
    pub name: String,
    pub measurement_type: MeasurementType,
}

#[derive(Debug, Clone, Copy)]
pub enum MeasurementType {
    Rapl,           // Intel RAPL
    HwMon,          // Linux hwmon
    BatteryMonitor, // Battery subsystem
    Estimate,       // Estimated via heuristics
}

pub type MeasurementStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = PowerMeasurement> + Send>>;

// Filesystem Abstraction for Drivers

#[async_trait]
pub trait Filesystem: Send + Sync {
    async fn read_to_string(&self, path: &std::path::Path) -> Result<String>;
    async fn write_all(&self, path: &std::path::Path, data: &[u8]) -> Result<()>;

    async fn read_u64(&self, path: &std::path::Path) -> Result<u64> {
        let s = self.read_to_string(path).await?;
        s.trim()
            .parse::<u64>()
            .map_err(|e| Error::Parse(e.to_string()))
    }
}

pub struct OsFs;

#[async_trait]
impl Filesystem for OsFs {
    async fn read_to_string(&self, path: &std::path::Path) -> Result<String> {
        Ok(tokio::fs::read_to_string(path).await?)
    }

    async fn write_all(&self, path: &std::path::Path, data: &[u8]) -> Result<()> {
        Ok(tokio::fs::write(path, data).await?)
    }
}

// Driver Registry

pub struct DriverRegistry {
    drivers: RwLock<Vec<Arc<dyn ComponentDriver>>>,
    power_meters: RwLock<Vec<Arc<dyn PowerMeter>>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self {
            drivers: RwLock::new(Vec::new()),
            power_meters: RwLock::new(Vec::new()),
        }
    }

    pub async fn register_driver(&self, driver: Arc<dyn ComponentDriver>) {
        self.drivers.write().await.push(driver);
    }

    pub async fn register_power_meter(&self, meter: Arc<dyn PowerMeter>) {
        self.power_meters.write().await.push(meter);
    }

    pub async fn discover_all(&self) -> Result<Vec<ComponentState>> {
        let drivers = self.drivers.read().await;
        let mut components = Vec::new();

        for driver in drivers.iter() {
            match driver.discover().await {
                Ok(mut comps) => components.append(&mut comps),
                Err(e) => tracing::warn!("Driver discovery failed: {}", e),
            }
        }

        Ok(components)
    }

    pub async fn get_driver(
        &self,
        component_type: ComponentType,
    ) -> Option<Arc<dyn ComponentDriver>> {
        let drivers = self.drivers.read().await;
        drivers
            .iter()
            .find(|d| d.component_type() == component_type)
            .cloned()
    }

    pub async fn power_meters(&self) -> Vec<Arc<dyn PowerMeter>> {
        self.power_meters.read().await.clone()
    }
}

// Linux-specific Implementations

#[cfg(feature = "linux")]
pub mod linux {
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    /// CPU driver using sysfs (simplified demo)
    pub struct CpuDriver<F: Filesystem = OsFs> {
        base_path: std::path::PathBuf,
        fs: std::sync::Arc<F>,
    }

    impl<F: Filesystem> CpuDriver<F> {
        pub fn new_with_fs(fs: std::sync::Arc<F>) -> Self {
            Self {
                base_path: "/sys/devices/system/cpu".into(),
                fs,
            }
        }

        async fn cpu_count(&self) -> Result<u32> {
            // read online mask: e.g. "0-15"
            let s = self
                .fs
                .read_to_string(&self.base_path.join("online"))
                .await?;
            let mut max = 0;
            for part in s.trim().split(',') {
                if let Some((_, hi)) = part.split_once('-') {
                    max = max.max(hi.parse::<u32>().unwrap_or(0));
                } else {
                    max = max.max(part.parse::<u32>().unwrap_or(0));
                }
            }
            Ok(max + 1)
        }
    }

    #[async_trait]
    impl<F: Filesystem + 'static> ComponentDriver for CpuDriver<F> {
        fn component_type(&self) -> ComponentType {
            ComponentType::Cpu
        }

        async fn discover(&self) -> Result<Vec<ComponentState>> {
            let n = self.cpu_count().await.unwrap_or(1);
            let mut v = Vec::with_capacity(n as usize);
            for cpu in 0..n {
                let id = ComponentId::new();
                let name = format!("cpu{cpu}");
                let current_gov = self
                    .fs
                    .read_to_string(
                        &self
                            .base_path
                            .join(format!("cpu{cpu}/cpufreq/scaling_governor")),
                    )
                    .await
                    .unwrap_or_else(|_| "unknown".into());
                let caps = vec![Capability::GovernorControl {
                    available: vec!["performance".into(), "powersave".into()],
                    current: current_gov,
                }];
                v.push(ComponentState {
                    id,
                    component_type: ComponentType::Cpu,
                    name,
                    power: None,
                    properties: ComponentProperties {
                        vendor: None,
                        model: None,
                        driver: Some("cpufreq".into()),
                        metadata: Default::default(),
                    },
                    capabilities: caps,
                });
            }
            Ok(v)
        }

        async fn read_state(&self, _id: ComponentId) -> Result<ComponentState> {
            Err(Error::Other(anyhow::anyhow!("read_state not implemented")))
        }

        async fn apply_capability(&self, _id: ComponentId, capability: Capability) -> Result<()> {
            if let Capability::GovernorControl { current, .. } = capability {
                let n = self.cpu_count().await?;
                for cpu in 0..n {
                    let path = self
                        .base_path
                        .join(format!("cpu{cpu}/cpufreq/scaling_governor"));
                    self.fs
                        .write_all(&path, format!("{current}\n").as_bytes())
                        .await?;
                }
                Ok(())
            } else {
                Err(Error::CapabilityNotSupported(format!("{capability:?}")))
            }
        }

        async fn monitor(&self, _id: ComponentId) -> Result<ComponentStream> {
            Err(Error::Other(anyhow::anyhow!("monitor not implemented")))
        }

        async fn validate_capability(
            &self,
            _id: ComponentId,
            capability: &Capability,
        ) -> Result<bool> {
            Ok(matches!(capability, Capability::GovernorControl { .. }))
        }
    }

    /// RAPL power meter (very simplified placeholder)
    pub struct RaplPowerMeter<F: Filesystem = OsFs> {
        base_path: std::path::PathBuf,
        fs: std::sync::Arc<F>,
    }

    impl Default for RaplPowerMeter<OsFs> {
        fn default() -> Self {
            Self {
                base_path: "/sys/class/powercap/intel-rapl".into(),
                fs: std::sync::Arc::new(OsFs),
            }
        }
    }

    impl CpuDriver<OsFs> {
        pub fn new() -> Self {
            Self::new_with_fs(std::sync::Arc::new(OsFs))
        }
    }

    impl<F: Filesystem + 'static> RaplPowerMeter<F> {
        async fn make_point(&self, rapl_dir: &std::path::Path) -> Result<MeasurementPoint> {
            let name = self
                .fs
                .read_to_string(&rapl_dir.join("name"))
                .await
                .unwrap_or_else(|_| "package-0".into());
            let id = ComponentId(Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                rapl_dir.as_os_str().as_bytes(),
            ));
            Ok(MeasurementPoint {
                id,
                name,
                measurement_type: MeasurementType::Rapl,
            })
        }
    }

    #[async_trait]
    impl<F: Filesystem + 'static> PowerMeter for RaplPowerMeter<F> {
        async fn measurement_points(&self) -> Result<Vec<MeasurementPoint>> {
            // For MVP, only intel-rapl:0 (extend later to scan rapl domains)
            let pkg = self.base_path.join("intel-rapl:0");
            if tokio::fs::try_exists(&pkg).await.unwrap_or(false) {
                Ok(vec![self.make_point(&pkg).await?])
            } else {
                Ok(vec![]) // no RAPL available
            }
        }

        async fn read_power(&self, point: &MeasurementPoint) -> Result<PowerMeasurement> {
            let rapl_dir = self.base_path.join("intel-rapl:0");
            let energy_path = rapl_dir.join("energy_uj");
            let max_path = rapl_dir.join("max_energy_range_uj");

            let t0 = chrono::Utc::now();
            let e0 = self
                .fs
                .read_to_string(&energy_path)
                .await?
                .trim()
                .parse::<u64>()
                .map_err(|e| Error::Parse(e.to_string()))?;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let t1 = chrono::Utc::now();
            let e1 = self
                .fs
                .read_to_string(&energy_path)
                .await?
                .trim()
                .parse::<u64>()
                .map_err(|e| Error::Parse(e.to_string()))?;
            let max = self
                .fs
                .read_to_string(&max_path)
                .await
                .unwrap_or_else(|_| "0".into())
                .trim()
                .parse::<u64>()
                .unwrap_or(0);

            let de_uj = if e1 >= e0 {
                e1 - e0
            } else if max > 0 {
                (max - e0) + e1
            } else {
                0
            } as f64;
            let dt_s = (t1 - t0).num_microseconds().unwrap_or(50_000) as f64 / 1_000_000.0;
            let watts = if dt_s > 0.0 {
                (de_uj / 1_000_000.0) / dt_s
            } else {
                0.0
            };

            Ok(PowerMeasurement {
                watts,
                uncertainty: 0.1,
                timestamp: t1,
            })
        }

        async fn start_monitoring(
            &self,
            point: MeasurementPoint,
            interval: std::time::Duration,
        ) -> Result<MeasurementStream> {
            let rapl_dir = self.base_path.join("intel-rapl:0");
            let energy_path = rapl_dir.join("energy_uj");
            let max_path = rapl_dir.join("max_energy_range_uj");
            let fs = self.fs.clone();

            let s = async_stream::stream! {
                let mut last_e: Option<u64> = None;
                let mut last_t: Option<chrono::DateTime<chrono::Utc>> = None;
                let max: u64 = fs.read_to_string(&max_path).await
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(0);

                loop {
                    let now = chrono::Utc::now();
                    let e = match fs.read_to_string(&energy_path).await
                        .ok()
                        .and_then(|s| s.trim().parse::<u64>().ok()) {
                            Some(v) => v,
                            None => { tokio::time::sleep(interval).await; continue; }
                        };

                    if let (Some(e0), Some(t0)) = (last_e, last_t) {
                        let de_uj = if e >= e0 { e - e0 } else if max > 0 { (max - e0) + e } else { 0 } as f64;
                        let dt_s = (now - t0).num_microseconds().unwrap_or(0) as f64 / 1_000_000.0;
                        if dt_s > 0.0 {
                            let watts = (de_uj / 1_000_000.0) / dt_s;
                            yield PowerMeasurement { watts, uncertainty: 0.1, timestamp: now };
                        }
                    }

                    last_e = Some(e);
                    last_t = Some(now);
                    tokio::time::sleep(interval).await;
                }
            };

            Ok(Box::pin(s))
        }
    }
}
