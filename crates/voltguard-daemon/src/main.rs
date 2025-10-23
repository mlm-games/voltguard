use std::os::fd::AsFd;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};
use voltguard_core::PowerManager;
use voltguard_engine::{MonitoringEngine, OptimizationEngine};
use voltguard_hal::DriverRegistry;
use voltguard_policy::PolicyEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Starting VoltGuard daemon");

    // Build the daemon
    let daemon = Daemon::new().await?;

    // Run the daemon
    daemon.run().await?;

    Ok(())
}

struct Daemon {
    power_manager: Arc<PowerManager>,
    monitoring_engine: Arc<MonitoringEngine>,
    optimization_engine: Arc<OptimizationEngine>,
    policy_engine: Arc<PolicyEngine>,
}

impl Daemon {
    async fn new() -> anyhow::Result<Self> {
        // Create driver registry and register drivers
        let registry = Arc::new(DriverRegistry::new());

        // Register platform-specific drivers
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
            // Register other drivers...
        }

        // Create core power manager
        let power_manager = Arc::new(PowerManager::new(registry));
        power_manager.initialize().await?;

        // Create engines
        let monitoring_engine = Arc::new(MonitoringEngine::new(power_manager.clone()));
        let metrics = monitoring_engine.metrics();
        let optimization_engine = Arc::new(OptimizationEngine::new(power_manager.clone(), metrics));
        let policy_engine = Arc::new(PolicyEngine::new(power_manager.clone()));

        Ok(Self {
            power_manager,
            monitoring_engine,
            optimization_engine,
            policy_engine,
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
        let ipc_server = IpcServer::new(self.power_manager.clone());
        tokio::spawn(async move {
            if let Err(e) = ipc_server.run().await {
                error!("IPC server error: {}", e);
            }
        });

        // Wait for shutdown signal
        info!("VoltGuard daemon running");
        signal::ctrl_c().await?;
        info!("Shutting down VoltGuard daemon");

        Ok(())
    }
}

// IPC Server for CLI Communication
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use nix::unistd::Uid;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use voltguard_api::{IpcRequest, IpcResponse, PowerProfile, ProtocolVersion, PROTOCOL};

struct IpcServer {
    power_manager: Arc<PowerManager>,
}

impl IpcServer {
    fn new(power_manager: Arc<PowerManager>) -> Self {
        Self { power_manager }
    }

    async fn run(&self) -> anyhow::Result<()> {
        let socket_path = "/var/run/voltguard.sock";

        // Remove old socket if exists
        let _ = std::fs::remove_file(socket_path);

        let listener = UnixListener::bind(socket_path)?;
        info!("IPC server listening on {}", socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let power_manager = self.power_manager.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, power_manager).await {
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

async fn handle_client(stream: UnixStream, power_manager: Arc<PowerManager>) -> anyhow::Result<()> {
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
        let request: IpcRequest = serde_json::from_str(&line)?;
        let response = handle_request(request, uid, &power_manager).await;
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
) -> IpcResponse {
    let is_root = uid.is_root();

    match request {
        IpcRequest::GetProfile => IpcResponse::Profile(power_manager.current_profile().await),
        IpcRequest::GetComponents => IpcResponse::Components(power_manager.get_components().await),
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
