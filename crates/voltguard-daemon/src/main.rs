use std::os::fd::AsFd;
use std::os::unix::fs::chown;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info, warn};
use voltguard_config as config;
use voltguard_core::PowerManager;
use voltguard_engine::{MonitoringEngine, OptimizationEngine};
use voltguard_hal::DriverRegistry;
use voltguard_policy::PolicyEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Starting VoltGuard daemon");

    let cfg = config::load().unwrap_or_else(|e| {
        warn!("Failed to load config, using defaults: {}", e);
        config::Config::default()
    });

    // Build the daemon
    let daemon = Daemon::new(cfg).await?;

    // Run the daemon
    daemon.run().await?;

    Ok(())
}

struct Daemon {
    power_manager: Arc<PowerManager>,
    monitoring_engine: Arc<MonitoringEngine>,
    optimization_engine: Arc<OptimizationEngine>,
    policy_engine: Arc<PolicyEngine>,
    socket_path: std::path::PathBuf,
}

impl Daemon {
    async fn new(cfg: config::Config) -> anyhow::Result<Self> {
        let registry = Arc::new(DriverRegistry::new());

        #[cfg(target_os = "linux")]
        {
            use std::sync::Arc as SyncArc;
            use voltguard_hal::linux::{CpuDriver, RaplPowerMeter};
            registry
                .register_driver(SyncArc::new(CpuDriver::new()))
                .await;
            registry
                .register_power_meter(SyncArc::new(RaplPowerMeter::default()))
                .await;
        }

        let power_manager = Arc::new(PowerManager::new(registry.clone()));
        power_manager.initialize().await?;

        let meters = registry.power_meters().await;
        let monitoring_engine = Arc::new(MonitoringEngine::new(power_manager.clone(), meters));
        let metrics = monitoring_engine.metrics();
        let optimization_engine = Arc::new(OptimizationEngine::new(
            power_manager.clone(),
            metrics.clone(),
        ));
        let policy_engine = Arc::new(PolicyEngine::new(power_manager.clone()));

        Ok(Self {
            power_manager,
            monitoring_engine,
            optimization_engine,
            policy_engine,
            socket_path: cfg.daemon.socket_path,
        })
    }

    async fn run(self) -> anyhow::Result<()> {
        // Start monitoring
        self.monitoring_engine.start().await?;

        // Start periodic policy enforcement
        let policy_engine = self.policy_engine.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                if let Err(e) = policy_engine.enforce().await {
                    error!("Policy enforcement error: {}", e);
                }
            }
        });

        // Start periodic optimization analysis (placeholder logs)
        let optimization_engine = self.optimization_engine.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let suggestions = optimization_engine.analyze_and_suggest().await;
                for suggestion in suggestions {
                    info!("Optimization suggestion: {:?}", suggestion);
                }
            }
        });

        // Setup IPC server for CLI communication
        let ipc_server = IpcServer::new(
            self.power_manager.clone(),
            self.monitoring_engine.metrics(),
            self.socket_path.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = ipc_server.run().await {
                error!("IPC server error: {}", e);
            }
        });

        // Wait for shutdown
        info!("VoltGuard daemon running");
        signal::ctrl_c().await?;
        info!("Shutting down VoltGuard daemon");
        Ok(())
    }
}

// IPC Server for CLI Communication

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use nix::unistd::{Gid, Uid};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use voltguard_api::{IpcRequest, IpcResponse, PowerProfile, ProtocolVersion, PROTOCOL};
use voltguard_engine::MetricsCollector;

struct IpcServer {
    power_manager: Arc<PowerManager>,
    metrics: Arc<MetricsCollector>,
    socket_path: std::path::PathBuf,
}

impl IpcServer {
    fn new(
        power_manager: Arc<PowerManager>,
        metrics: Arc<MetricsCollector>,
        socket_path: std::path::PathBuf,
    ) -> Self {
        Self {
            power_manager,
            metrics,
            socket_path,
        }
    }

    async fn run(&self) -> anyhow::Result<()> {
        let socket_path = &self.socket_path;

        // Remove old socket if exists
        let _ = std::fs::remove_file(socket_path);

        let listener = UnixListener::bind(socket_path)?;
        info!("IPC server listening on {}", socket_path.display());

        // Tighten perms: 0660; optionally honor VOLTGUARD_GROUP for group ownership
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))?;
        if let Ok(group) = std::env::var("VOLTGUARD_GROUP") {
            if let Ok(g) = group.parse::<u32>() {
                let _ = chown(socket_path, None, Some(Gid::from_raw(g).into()));
            }
        }

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let power_manager = self.power_manager.clone();
                    let metrics = self.metrics.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, power_manager, metrics).await {
                            error!("Client handler error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Accept error: {}", e);
                }
            }
        }
    }
}

async fn handle_client(
    stream: UnixStream,
    power_manager: Arc<PowerManager>,
    metrics: Arc<MetricsCollector>,
) -> anyhow::Result<()> {
    let fd = stream.as_fd();
    let creds = getsockopt(&fd, PeerCredentials)?;
    let uid = Uid::from_raw(creds.uid());

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Require Hello first
    reader.read_line(&mut line).await?;
    let hello: IpcRequest = serde_json::from_str(&line)?;
    if let IpcRequest::Hello(ver) = hello {
        if ver.major != PROTOCOL.major {
            respond(
                &mut writer,
                &IpcResponse::Error("Protocol major mismatch".into()),
            )
            .await?;
            return Ok(());
        }
        respond(&mut writer, &IpcResponse::HelloAck(PROTOCOL)).await?;
    } else {
        respond(&mut writer, &IpcResponse::Error("Expected Hello".into())).await?;
        return Ok(());
    }
    line.clear();

    // Process requests
    while reader.read_line(&mut line).await? > 0 {
        let request: IpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                respond(
                    &mut writer,
                    &IpcResponse::Error(format!("Parse error: {}", e)),
                )
                .await?;
                line.clear();
                continue;
            }
        };
        let response = handle_request(request, uid, &power_manager, &metrics).await;
        respond(&mut writer, &response).await?;
        line.clear();
    }

    Ok(())
}

async fn respond<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &IpcResponse,
) -> anyhow::Result<()> {
    let mut buf = serde_json::to_vec(response)?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    Ok(())
}

async fn handle_request(
    request: IpcRequest,
    uid: Uid,
    power_manager: &PowerManager,
    metrics: &MetricsCollector,
) -> IpcResponse {
    let is_root = uid.is_root();

    match request {
        IpcRequest::GetProfile => IpcResponse::Profile(power_manager.current_profile().await),
        IpcRequest::GetComponents => IpcResponse::Components(power_manager.get_components().await),
        IpcRequest::GetTotalPower => {
            let total = metrics.get_total_power().await;
            IpcResponse::TotalPower(total)
        }
        IpcRequest::SetProfile(profile) if is_root => {
            match power_manager.set_profile(profile).await {
                Ok(()) => IpcResponse::Success,
                Err(e) => IpcResponse::Error(e.to_string()),
            }
        }
        IpcRequest::ApplyCapability { id, capability } if is_root => {
            match power_manager.apply_capability(id, capability).await {
                Ok(()) => IpcResponse::Success,
                Err(e) => IpcResponse::Error(e.to_string()),
            }
        }
        IpcRequest::SetProfile(_) | IpcRequest::ApplyCapability { .. } => {
            IpcResponse::Error("Permission denied".into())
        }
        IpcRequest::Hello(_v) => IpcResponse::Error("Already said hello".into()),
    }
}
