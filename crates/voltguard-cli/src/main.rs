use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use voltguard_api::*;

#[derive(Parser)]
#[clap(name = "voltguard", about = "Modern power management for Linux")]
struct Cli {
    /// Override socket path (arg > env > config)
    #[clap(long)]
    socket: Option<PathBuf>,

    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show current power consumption and component status
    Status,

    /// List all detected components
    List,

    /// Get or set power profile
    Profile {
        /// New profile to set (performance, balanced, powersave)
        #[clap(value_parser)]
        profile: Option<String>,
    },

    /// Show power optimization suggestions (placeholder)
    Suggest,

    /// Apply optimization (placeholder)
    Optimize {
        /// Apply suggestions automatically
        #[clap(short, long)]
        auto: bool,
    },

    /// Monitor power consumption in real-time (placeholder)
    Monitor {
        /// Update interval in seconds
        #[clap(short, long, default_value = "1")]
        interval: u64,
    },

    /// Generate power usage report (placeholder)
    Report {
        /// Output format (text, json, html)
        #[clap(short, long, default_value = "text")]
        format: String,

        /// Output file
        #[clap(short, long)]
        output: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut client = IpcClient::connect(cli.socket).await?;

    match cli.command {
        Commands::Status => {
            show_status(&mut client).await?;
        }
        Commands::List => {
            list_components(&mut client).await?;
        }
        Commands::Profile { profile } => {
            handle_profile(&mut client, profile).await?;
        }
        Commands::Suggest => {
            println!("Suggestions not implemented yet.");
        }
        Commands::Optimize { auto } => {
            println!("Optimize (auto={}) not implemented yet.", auto);
        }
        Commands::Monitor { interval } => {
            println!("Monitor (interval={}s) not implemented yet.", interval);
        }
        Commands::Report { format, output } => {
            println!(
                "Report (format={}, output={:?}) not implemented yet.",
                format, output
            );
        }
    }

    Ok(())
}

async fn show_status(client: &mut IpcClient) -> anyhow::Result<()> {
    let profile = client.get_profile().await?;
    let components = client.get_components().await?;
    let total_power = client.get_total_power().await.unwrap_or_else(|_| 0.0);

    println!("VoltGuard Status");
    println!("================");
    println!("Power Profile: {:?}", profile);
    println!();
    println!("Total Power (pkg): {:.2} W", total_power);
    println!();

    println!("Components:");
    for component in components {
        println!("  {} ({:?})", component.name, component.component_type);
        if let Some(power) = component.power {
            println!("    Power: {:.2} W", power.watts);
        }
    }
    Ok(())
}

async fn list_components(client: &mut IpcClient) -> anyhow::Result<()> {
    let components = client.get_components().await?;

    for component in components {
        println!("{:?} - {}", component.component_type, component.name);
        println!("  ID: {:?}", component.id);

        if let Some(vendor) = &component.properties.vendor {
            println!("  Vendor: {}", vendor);
        }
        if let Some(model) = &component.properties.model {
            println!("  Model: {}", model);
        }

        println!("  Capabilities:");
        for cap in &component.capabilities {
            println!("    - {:?}", cap);
        }
        println!();
    }

    Ok(())
}

async fn handle_profile(client: &mut IpcClient, profile: Option<String>) -> anyhow::Result<()> {
    if let Some(profile_name) = profile {
        let profile = match profile_name.to_lowercase().as_str() {
            "performance" => PowerProfile::Performance,
            "balanced" => PowerProfile::Balanced,
            "powersave" => PowerProfile::PowerSave,
            _ => {
                eprintln!("Invalid profile. Valid options: performance, balanced, powersave");
                return Ok(());
            }
        };

        client.set_profile(profile).await?;
        println!("Profile set to: {:?}", profile);
    } else {
        let profile = client.get_profile().await?;
        println!("Current profile: {:?}", profile);
    }

    Ok(())
}

/// IPC Client...
struct IpcClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl IpcClient {
    async fn connect(cli_socket: Option<PathBuf>) -> anyhow::Result<Self> {
        let cfg = voltguard_config::load().unwrap_or_default();
        let from_cfg = cfg.daemon.socket_path;
        let path = cli_socket
            .or_else(|| std::env::var_os("VOLTGUARD_SOCK").map(PathBuf::from))
            .unwrap_or(from_cfg);

        let stream = UnixStream::connect(&path).await?;
        let (r, w) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(r),
            writer: w,
        };
        client.hello().await?;
        Ok(client)
    }

    async fn hello(&mut self) -> anyhow::Result<()> {
        self.send(&IpcRequest::Hello(PROTOCOL)).await?;
        match self.recv().await? {
            IpcResponse::HelloAck(version) if version.major == PROTOCOL.major => Ok(()),
            IpcResponse::Error(e) => anyhow::bail!("IPC Hello error: {e}"),
            other => anyhow::bail!("Unexpected Hello response: {other:?}"),
        }
    }

    async fn send(&mut self, req: &IpcRequest) -> anyhow::Result<()> {
        let mut buf = serde_json::to_vec(req)?;
        buf.push(b'\n');
        self.writer.write_all(&buf).await?;
        Ok(())
    }

    async fn recv(&mut self) -> anyhow::Result<IpcResponse> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        Ok(serde_json::from_str(&line)?)
    }

    async fn get_profile(&mut self) -> anyhow::Result<PowerProfile> {
        self.send(&IpcRequest::GetProfile).await?;
        match self.recv().await? {
            IpcResponse::Profile(p) => Ok(p),
            IpcResponse::Error(e) => anyhow::bail!(e),
            r => anyhow::bail!("unexpected response: {r:?}"),
        }
    }

    async fn set_profile(&mut self, profile: PowerProfile) -> anyhow::Result<()> {
        self.send(&IpcRequest::SetProfile(profile)).await?;
        match self.recv().await? {
            IpcResponse::Success => Ok(()),
            IpcResponse::Error(e) => anyhow::bail!(e),
            r => anyhow::bail!("unexpected response: {r:?}"),
        }
    }

    async fn get_components(&mut self) -> anyhow::Result<Vec<ComponentState>> {
        self.send(&IpcRequest::GetComponents).await?;
        match self.recv().await? {
            IpcResponse::Components(c) => Ok(c),
            IpcResponse::Error(e) => anyhow::bail!(e),
            r => anyhow::bail!("unexpected response: {r:?}"),
        }
    }

    async fn get_total_power(&mut self) -> anyhow::Result<f64> {
        self.send(&IpcRequest::GetTotalPower).await?;
        match self.recv().await? {
            IpcResponse::TotalPower(v) => Ok(v),
            IpcResponse::Error(e) => anyhow::bail!(e),
            r => anyhow::bail!("unexpected response: {r:?}"),
        }
    }
}
