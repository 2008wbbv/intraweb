//! intraweb -- your neighborhood web.

mod api;
mod surf;
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
    /// Publish a folder to the neighborhood without copying it into your vault.
    Serve {
        /// The directory to publish as your site.
        #[arg(value_name = "DIR")]
        dir: PathBuf,

        #[command(flatten)]
        args: UpArgs,
    },
    /// See what your neighbors are publishing.
    Surf {
        /// Seconds to listen for neighbors before asking what they serve.
        #[arg(long, default_value_t = 3)]
        listen: u64,

        /// Open this numbered site in a browser instead of only listing.
        #[arg(long, value_name = "N")]
        open: Option<usize>,
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

    /// Publish this folder instead of the vault's own site/ directory.
    #[arg(long, value_name = "DIR")]
    site: Option<PathBuf>,
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
        site: None,
    })) {
        Command::Up(args) => up(vault, args).await,
        Command::Serve { dir, mut args } => {
            args.site = Some(dir);
            up(vault, args).await
        }
        Command::Surf { listen, open } => surf_command(vault, listen, open).await,
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

    // A served folder is published in place; nothing is copied into the vault,
    // so pointing at a directory never disturbs what you normally publish.
    let site_dir = match &args.site {
        Some(dir) => {
            let dir = dir.canonicalize().with_context(|| {
                format!("could not find the folder to serve: {}", dir.display())
            })?;
            anyhow::ensure!(dir.is_dir(), "{} is not a directory", dir.display());
            dir
        }
        None => vault.site_dir(),
    };

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
        site_dir: site_dir.clone(),
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
        println!("  serving    {}", site_dir.display());
        println!("  dashboard  {where_to_go}");
        println!("  your site  {where_to_go}/~{}", config.sanitized_nickname());
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

/// Look around the neighborhood and report what is being published.
///
/// This listens without announcing: looking at what neighbors serve should not
/// change what they see, and it keeps a node already running on this machine
/// from colliding with a second announcement under the same key.
async fn surf_command(vault: Vault, listen: u64, open: Option<usize>) -> Result<()> {
    let (identity, config, _) = open_vault(&vault)?;
    let store = Arc::new(Mutex::new(Store::open(&vault.db_path())?));
    let config = Arc::new(config);

    let node = Node::observe(&config, Arc::clone(&identity), Arc::clone(&store))
        .context("could not listen for neighbors")?;

    println!("Looking around the neighborhood for {listen}s...");
    tokio::time::sleep(Duration::from_secs(listen)).await;

    let peers = node.roster.peers();
    node.shutdown();

    if peers.is_empty() {
        println!("\nNobody is publishing anything right now.");
        println!("If you expected company, run `intraweb doctor` -- an empty list");
        println!("usually means the access point, not a broken node.");
        return Ok(());
    }

    let sites = surf::survey(peers, surf::PROBE_TIMEOUT).await;
    // Reachable sites first; nothing is more annoying than a listing led by
    // things you cannot open.
    let mut sites = sites;
    sites.sort_by(|a, b| {
        b.reachable
            .cmp(&a.reachable)
            .then_with(|| b.is_hub.cmp(&a.is_hub))
            .then_with(|| a.nickname.cmp(&b.nickname))
    });

    if let Some(choice) = open {
        let Some(site) = choice.checked_sub(1).and_then(|i| sites.get(i)) else {
            anyhow::bail!("there is no site {choice}; run `intraweb surf` to see the list");
        };
        println!("Opening {}", site.url);
        return surf::open_in_browser(&site.url);
    }

    let width = sites.iter().map(|s| s.nickname.chars().count()).max().unwrap_or(8).max(8);
    println!();
    for (index, site) in sites.iter().enumerate() {
        let marker = if site.is_hub { "*" } else { " " };
        println!(
            "{marker} {:>2}  {:<width$}  {:<34}  {}",
            index + 1,
            site.nickname,
            site.describe(),
            site.url,
        );
        // Never let a familiar-looking name stand on its own when the key
        // behind it has changed.
        if let Some(warning) = site.warning() {
            println!("      {:<width$}  !! {warning}", "");
        }
    }

    let unreachable = sites.iter().filter(|s| !s.reachable).count();
    println!();
    if sites.iter().any(|s| s.is_hub) {
        println!("* hub");
    }
    if unreachable > 0 {
        println!(
            "{unreachable} neighbor(s) are announcing but not answering -- they may have just left."
        );
    }
    println!("Open one with: intraweb surf --open <number>");
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
