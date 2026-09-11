//! intraweb -- your neighborhood web.

mod api;
mod tui;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use intraweb_core::peer::now_secs;
use intraweb_core::{Config, Identity, Store, Vault};
use intraweb_net::Node;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "intraweb",
    version,
    about = "your neighborhood web",
    long_about = "A local-first intranet. Your identity is a keypair and one folder; \
                  hubs are places to meet, not servers that own your data."
)]
struct Cli {
    /// Where your vault lives. Defaults to $INTRAWEB_VAULT, then ~/.intraweb
    #[arg(long, global = true, value_name = "DIR")]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the node: announce yourself, find neighbors, serve your vault.
    Up(UpArgs),
    /// Diagnose why neighbors are or are not showing up.
    Doctor {
        /// Seconds to listen on both discovery paths.
        #[arg(long, default_value_t = 5)]
        listen: u64,
    },
    /// Print your identity and where your vault lives.
    Id,
}

#[derive(Args)]
struct UpArgs {
    /// Host a hub: claim intranet.local and be a meeting point for the network.
    #[arg(long)]
    hub: bool,

    /// Name shown for the hub you are hosting.
    #[arg(long, value_name = "NAME")]
    hub_name: Option<String>,

    /// Display name. Your key stays the same whatever you call yourself.
    #[arg(long, value_name = "NAME")]
    nickname: Option<String>,

    /// Port for the dashboard and JSON API.
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,

    /// Show the terminal dashboard instead of log output.
    #[arg(long)]
    tui: bool,

    /// Turn off UDP broadcast and rely on mDNS alone.
    #[arg(long)]
    no_beacon: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let vault = Vault::resolve(cli.vault)?;

    match cli.command.unwrap_or(Command::Up(UpArgs {
        hub: false,
        hub_name: None,
        nickname: None,
        port: None,
        tui: false,
        no_beacon: false,
    })) {
        Command::Up(args) => up(vault, args).await,
        Command::Doctor { listen } => doctor(vault, listen).await,
        Command::Id => show_id(vault),
    }
}

/// Prepare the vault: create the layout, load or mint the identity, read config.
fn open_vault(vault: &Vault) -> Result<(Arc<Identity>, Config, bool)> {
    vault.ensure()?;
    let (identity, minted) = Identity::load_or_create(&vault.identity_path())?;
    let config = Config::load_or_create(&vault.config_path())?;
    Ok((Arc::new(identity), config, minted))
}

async fn up(vault: Vault, args: UpArgs) -> Result<()> {
    if !args.tui {
        init_logging();
    }

    let (identity, mut config, minted) = open_vault(&vault)?;

    // Flags win over the stored config, but are not written back: a one-off
    // `--hub` should not silently turn this node into a hub forever.
    if args.hub {
        config.hub = true;
    }
    if let Some(name) = args.hub_name {
        config.hub_name = name;
    }
    if let Some(nickname) = args.nickname {
        config.nickname = nickname;
    }
    // Remember that the operator picked this, so the hub's port-80 preference
    // does not quietly move the dashboard out from under them.
    let port_was_chosen = args.port.is_some() || config.api_port != intraweb_core::config::DEFAULT_API_PORT;
    if let Some(port) = args.port {
        config.api_port = port;
    }
    if args.no_beacon {
        config.beacon_enabled = false;
    }

    let store = Arc::new(Mutex::new(Store::open(&vault.db_path())?));
    let config = Arc::new(config);
    let vault = Arc::new(vault);

    if minted {
        println!("Minted a new identity: {}", identity.fingerprint());
        println!("Back up {} -- it is your name on every hub.\n", vault.root().display());
    }

    let node = Node::start(&config, Arc::clone(&identity), Arc::clone(&store))
        .context("could not start discovery")?;

    let state = api::AppState {
        roster: node.roster.clone(),
        identity: Arc::clone(&identity),
        store: Arc::clone(&store),
        config: Arc::clone(&config),
        vault: Arc::clone(&vault),
        started_at: now_secs(),
    };

    // A hub tries port 80 so newcomers can type a bare hostname -- unless a
    // port was chosen, in which case that one is honored.
    let (listener, port) =
        api::bind(api::should_try_low_port(config.hub, port_was_chosen), config.api_port).await?;
    let where_to_go = if config.hub {
        format!("http://intranet.local{}", suffix(port))
    } else {
        format!("http://localhost{}", suffix(port))
    };

    if args.tui {
        let dashboard = tui::Dashboard::new(
            node.roster.clone(),
            Arc::clone(&identity),
            Arc::clone(&store),
            Arc::clone(&config),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, api::router(state)).await;
        });
        // The terminal loop blocks, so it gets a thread of its own.
        tokio::task::spawn_blocking(move || dashboard.run()).await??;
        server.abort();
    } else {
        println!("intraweb -- your neighborhood web");
        println!("  you        {} ({})", config.sanitized_nickname(), identity.fingerprint());
        println!("  vault      {}", vault.root().display());
        println!("  dashboard  {where_to_go}");
        if config.hub {
            println!("  hosting    {}", config.hub_name);
        }
        println!("\nListening for neighbors. Ctrl-C to stop.\n");

        let server = axum::serve(listener, api::router(state));
        tokio::select! {
            result = server => result.context("the dashboard stopped unexpectedly")?,
            _ = tokio::signal::ctrl_c() => println!("\nLeaving the neighborhood."),
        }
    }

    node.shutdown();
    Ok(())
}

async fn doctor(vault: Vault, listen: u64) -> Result<()> {
    let (_identity, config, _) = open_vault(&vault)?;

    println!("Listening for {listen}s on both discovery paths...\n");
    let report = intraweb_net::doctor::run(config.beacon_port, Duration::from_secs(listen)).await?;

    for check in &report.checks {
        let mark = match check.status {
            intraweb_net::doctor::Status::Pass => "ok  ",
            intraweb_net::doctor::Status::Warn => "warn",
            intraweb_net::doctor::Status::Fail => "FAIL",
        };
        println!("  [{mark}] {:<18} {}", check.name, check.detail);
        if let Some(remedy) = &check.remedy {
            println!("         {remedy}");
        }
    }

    println!("\n{}", report.verdict);

    // A network that simply has nobody on it is not an error exit.
    if report.worst_status() == intraweb_net::doctor::Status::Fail {
        std::process::exit(1);
    }
    Ok(())
}

fn show_id(vault: Vault) -> Result<()> {
    let (identity, config, minted) = open_vault(&vault)?;
    if minted {
        println!("Minted a new identity.\n");
    }
    println!("nickname     {}", config.sanitized_nickname());
    println!("fingerprint  {}", identity.fingerprint());
    println!("public key   {}", identity.peer_id().to_hex());
    println!("vault        {}", vault.root().display());
    println!("\nRead the fingerprint aloud to a neighbor to verify each other.");
    Ok(())
}

/// Omit the port from URLs when it is the default, so the printed address is
/// the one a person would actually type.
fn suffix(port: u16) -> String {
    if port == 80 { String::new() } else { format!(":{port}") }
}

fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_env("INTRAWEB_LOG")
        .unwrap_or_else(|_| EnvFilter::new("intraweb_net=info,intraweb_cli=info,warn"));
    fmt().with_env_filter(filter).with_target(false).compact().init();
}
