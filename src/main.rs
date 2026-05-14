use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, ExitStatus};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use agentnoise::app::{AgentNoise, NewSessionRequest, RouteAction};
use agentnoise::auth::PairingGate;
use agentnoise::config::Config;
use agentnoise::desktop_alert;
use agentnoise::doctor::render_doctor;
use agentnoise::identity;
use agentnoise::launchd;
use agentnoise::runner::{AgentKind, AgentRequest};
use agentnoise::secrets;
use agentnoise::service::{self, ServiceTarget};
use agentnoise::setup::{self, SetupOptions, SetupResult};
use agentnoise::whitenoise_cli::{self, WhitenoiseInstall};
use agentnoise::wn::WnClient;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use zeroize::Zeroize;

#[derive(Clone)]
struct PairingRuntime {
    gate: PairingGate,
    payload: identity::PairingPayload,
}

#[derive(Debug, Parser)]
#[command(name = "agentnoise")]
#[command(about = "Chat with local coding agents through White Noise")]
struct Cli {
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(InitArgs),
    #[command(about = "Create the desktop identity, write config, and show the phone QR")]
    Setup(SetupArgs),
    #[command(about = "Set up AgentNoise, discover control chats, and listen")]
    Up(UpArgs),
    #[command(about = "Show the phone pairing QR for the desktop identity")]
    Pair(PairArgs),
    Doctor,
    Config(ConfigArgs),
    Parse {
        message: String,
    },
    Handle {
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        sender: Option<String>,
        message: String,
    },
    Run {
        agent: AgentKind,
        repo: String,
        #[arg(required = true, trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    #[command(about = "Start the daemon/login repair and listen for White Noise commands")]
    Start(StartArgs),
    Listen,
    Send {
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    #[command(about = "Manage native launchd/systemd/rc supervisor files")]
    Service(ServiceArgs),
    Launchd(LaunchdArgs),
    Whitenoise(WhitenoiseArgs),
    #[command(about = "Create and pair AgentNoise identities")]
    Identity(IdentityArgs),
    #[command(about = "Manage the AgentNoise OS keychain bootstrap secret")]
    Keychain(KeychainArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(
        long,
        help = "Phone White Noise npub; creates a control chat when provided"
    )]
    phone: Option<String>,
    #[arg(long, default_value = setup::DEFAULT_GROUP_NAME)]
    group_name: String,
    #[arg(long)]
    force_identity: bool,
    #[arg(long = "relay")]
    relays: Vec<String>,
}

#[derive(Debug, Args)]
struct UpArgs {
    #[arg(
        long,
        help = "Phone White Noise npub; creates a control chat when provided"
    )]
    phone: Option<String>,
    #[arg(long, default_value = setup::DEFAULT_GROUP_NAME)]
    group_name: String,
    #[arg(long, help = "Add a White Noise group id before starting")]
    group: Option<String>,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(long, help = "Stop after setup/group discovery instead of listening")]
    no_listen: bool,
    #[arg(long, help = "Do not start wn daemon before listening")]
    no_daemon: bool,
}

#[derive(Debug, Args)]
struct PairArgs {
    #[arg(long = "relay")]
    relays: Vec<String>,
}

#[derive(Debug, Args)]
struct StartArgs {
    #[arg(long, help = "Add a White Noise group id before starting")]
    group: Option<String>,
    #[arg(long, help = "Do not start wn daemon automatically")]
    no_daemon: bool,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Path,
    PrintTemplate,
}

#[derive(Debug, Args)]
struct LaunchdArgs {
    #[command(subcommand)]
    command: LaunchdCommand,
}

#[derive(Debug, Args)]
struct WhitenoiseArgs {
    #[command(subcommand)]
    command: WhitenoiseCommand,
}

#[derive(Debug, Args)]
struct IdentityArgs {
    #[command(subcommand)]
    command: IdentityCommand,
}

#[derive(Debug, Args)]
struct KeychainArgs {
    #[command(subcommand)]
    command: KeychainCommand,
}

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum WhitenoiseCommand {
    #[command(about = "Show resolved White Noise CLI paths and daemon state")]
    Status,
    #[command(about = "Print the resolved wn path")]
    Path,
    #[command(about = "Install wn and wnd under AgentNoise's managed data directory")]
    Install {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    #[command(about = "Show wn daemon status")]
    DaemonStatus,
    #[command(about = "Run wn login using the nsec stored in the OS keychain")]
    LoginFromKeychain {
        #[arg(long)]
        relay: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum LaunchdCommand {
    Print,
    Install {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        load: bool,
    },
    Uninstall {
        #[arg(long)]
        unload: bool,
    },
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    #[command(about = "Generate one or more Nostr identities and store nsecs in the OS keychain")]
    Create {
        #[arg(long, default_value = "desktop")]
        name: String,
        #[arg(long, default_value_t = 1)]
        count: usize,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Show a stored identity's public key")]
    Show {
        #[arg(long, default_value = "desktop")]
        name: String,
    },
    #[command(about = "Render a phone-pairing QR for a stored identity")]
    Qr {
        #[arg(long, default_value = "desktop")]
        name: String,
        #[arg(long = "relay")]
        relays: Vec<String>,
    },
    #[command(about = "Delete a named identity nsec from the OS keychain")]
    Delete {
        #[arg(long, default_value = "desktop")]
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum KeychainCommand {
    #[command(about = "Store a White Noise nsec in the OS keychain")]
    StoreNsec,
    #[command(about = "Check whether the OS keychain contains an AgentNoise nsec")]
    Status,
    #[command(about = "Delete the stored White Noise nsec from the OS keychain")]
    DeleteNsec,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    #[command(about = "Print a native supervisor unit/service file")]
    Print {
        #[arg(long)]
        target: Option<ServiceTarget>,
    },
    #[command(about = "Install a native supervisor unit/service file")]
    Install {
        #[arg(long)]
        target: Option<ServiceTarget>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        load: bool,
        #[arg(long)]
        path: Option<PathBuf>,
    },
    #[command(about = "Remove a native supervisor unit/service file")]
    Uninstall {
        #[arg(long)]
        target: Option<ServiceTarget>,
        #[arg(long)]
        unload: bool,
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = Config::path_or_default(cli.config);

    match cli.command {
        Command::Init(args) => {
            Config::write_template(&config_path, args.force)?;
            println!("wrote {}", config_path.display());
        }
        Command::Setup(args) => {
            let result = setup::setup(
                &config_path,
                SetupOptions {
                    phone_npub: args.phone,
                    group_name: args.group_name,
                    force_identity: args.force_identity,
                    relays: args.relays,
                },
            )?;
            print_setup_result(&result);
        }
        Command::Up(args) => {
            up(&config_path, args)?;
        }
        Command::Pair(args) => {
            let payload = setup::pairing(&config_path, &args.relays)?;
            println!("agentnoise pairing");
            println!("npub: {}", payload.npub);
            println!("nprofile: {}", payload.nprofile);
            println!();
            println!("{}", identity::render_qr(&payload.nprofile)?);
        }
        Command::Doctor => {
            let config = Config::load_or_template(&config_path)?;
            println!("{}", render_doctor(&config_path, &config));
        }
        Command::Config(args) => match args.command {
            ConfigCommand::Path => println!("{}", config_path.display()),
            ConfigCommand::PrintTemplate => println!("{}", Config::template_toml()?),
        },
        Command::Parse { message } => {
            let command = agentnoise::chat::parse_chat_command(&message)?;
            println!("{command:#?}");
        }
        Command::Handle {
            group,
            sender,
            message,
        } => {
            let app = AgentNoise::from_config_path(&config_path)?;
            match app.route_message(group.as_deref(), sender.as_deref(), &message)? {
                RouteAction::Ignore => {}
                RouteAction::Reply(reply) => println!("{reply}"),
                RouteAction::NewSession(request) => {
                    println!("New session requested: {}", request.name);
                    println!(
                        "Run this command from the live listener so AgentNoise can create the White Noise chat."
                    );
                }
                RouteAction::ResumeSession(request) => {
                    println!("{}", request.reply_text);
                    println!("Target chat: {}", request.group_id);
                    println!("{}", request.target_text);
                }
                RouteAction::Run(request) => println!("{}", app.run_request(request)?),
            }
        }
        Command::Run {
            agent,
            repo,
            prompt,
        } => {
            let app = AgentNoise::from_config_path(&config_path)?;
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                bail!("prompt cannot be empty");
            }
            let request = AgentRequest::new(agent, repo, prompt);
            println!("{}", app.run_request(request)?);
        }
        Command::Start(args) => {
            start_listener(&config_path, args)?;
        }
        Command::Listen => {
            let config = Config::load(&config_path)?;
            let pairing = pairing_for_listener(&config_path, &config)?;
            run_listener(&config_path, config, pairing)?;
        }
        Command::Send { text } => {
            let config = Config::load(&config_path)?;
            if whitenoise_cli::ensure_login_from_keychain(&config.whitenoise)? {
                eprintln!("agentnoise: restored White Noise login from OS keychain");
            }
            let wn = WnClient::new(config.whitenoise);
            wn.send_reply(&text.join(" "))?;
        }
        Command::Service(args) => {
            service_command(&config_path, args)?;
        }
        Command::Launchd(args) => match args.command {
            LaunchdCommand::Print => {
                let config = Config::load_or_template(&config_path)?;
                let exe = std::env::current_exe().context("resolving current executable")?;
                println!("{}", launchd::render_plist(&exe, &config_path, &config));
            }
            LaunchdCommand::Install { force, load } => {
                let config = Config::load(&config_path)?;
                let exe = std::env::current_exe().context("resolving current executable")?;
                let path = launchd::install(&exe, &config_path, &config, force)?;
                if load {
                    launchd::load_plist(&path)?;
                }
                println!("installed {}", path.display());
            }
            LaunchdCommand::Uninstall { unload } => {
                let removed = launchd::uninstall(unload)?;
                if removed {
                    println!("removed {}", launchd::plist_path().display());
                } else {
                    println!("not installed");
                }
            }
        },
        Command::Whitenoise(args) => {
            let config = Config::load_or_template(&config_path)?;
            match args.command {
                WhitenoiseCommand::Status => {
                    println!("{}", whitenoise_cli::render_status(&config.whitenoise));
                }
                WhitenoiseCommand::Path => {
                    println!(
                        "{}",
                        whitenoise_cli::resolve_wn(&config.whitenoise.wn_bin).display()
                    );
                }
                WhitenoiseCommand::Install { force, root } => {
                    let options = WhitenoiseInstall {
                        root: root.unwrap_or_else(agentnoise::paths::managed_whitenoise_root),
                        force,
                    };
                    whitenoise_cli::install(&options)?;
                    println!("installed White Noise CLI under {}", options.root.display());
                }
                WhitenoiseCommand::DaemonStatus => {
                    let wn = whitenoise_cli::resolve_wn(&config.whitenoise.wn_bin);
                    println!(
                        "{}",
                        whitenoise_cli::daemon_status_with_socket(
                            &wn,
                            config.whitenoise.resolved_socket().as_deref()
                        )?
                    );
                }
                WhitenoiseCommand::LoginFromKeychain { relay } => {
                    let output =
                        whitenoise_cli::login_from_keychain(&config.whitenoise, relay.as_deref())?;
                    if output.is_empty() {
                        println!("logged in from OS keychain");
                    } else {
                        println!("{output}");
                    }
                }
            }
        }
        Command::Identity(args) => {
            let config = Config::load_or_template(&config_path)?;
            match args.command {
                IdentityCommand::Create { name, count, force } => {
                    let identities =
                        identity::create_identities(&config.whitenoise, &name, count, force)?;
                    println!("stored AgentNoise identity nsecs in OS keychain");
                    for identity in identities {
                        println!();
                        println!("name: {}", identity.name);
                        println!("npub: {}", identity.npub);
                        println!(
                            "keychain: {} / {}",
                            config.whitenoise.keychain_service, identity.keychain_item
                        );
                    }
                }
                IdentityCommand::Show { name } => {
                    let public = identity::load_public_identity(&config.whitenoise, &name)?;
                    println!("name: {}", public.name);
                    println!("npub: {}", public.npub);
                    println!(
                        "keychain: {} / {}",
                        config.whitenoise.keychain_service, public.keychain_item
                    );
                }
                IdentityCommand::Qr { name, relays } => {
                    let payload = identity::pairing_payload(&config.whitenoise, &name, &relays)?;
                    println!("agentnoise pairing");
                    println!("name: {}", payload.name);
                    println!("npub: {}", payload.npub);
                    println!("nprofile: {}", payload.nprofile);
                    println!("relays:");
                    for relay in &payload.relays {
                        println!("- {relay}");
                    }
                    println!();
                    println!("{}", identity::render_qr(&payload.nprofile)?);
                }
                IdentityCommand::Delete { name } => {
                    let store = identity::identity_store(&config.whitenoise, &name);
                    store.delete_nsec()?;
                    println!(
                        "deleted AgentNoise identity nsec from OS keychain: {}",
                        store.label()
                    );
                }
            }
        }
        Command::Keychain(args) => {
            let config = Config::load_or_template(&config_path)?;
            let store = config.secret_store();
            match args.command {
                KeychainCommand::StoreNsec => {
                    let mut nsec = secrets::read_nsec_interactive()?;
                    store.store_nsec(&nsec)?;
                    nsec.zeroize();
                    println!("stored White Noise nsec in OS keychain: {}", store.label());
                }
                KeychainCommand::Status => {
                    let status = if store.nsec_status()? {
                        "present"
                    } else {
                        "missing"
                    };
                    println!("{}: {status}", store.label());
                }
                KeychainCommand::DeleteNsec => {
                    store.delete_nsec()?;
                    println!(
                        "deleted White Noise nsec from OS keychain: {}",
                        store.label()
                    );
                }
            }
        }
    }

    Ok(())
}

fn print_setup_result(result: &SetupResult) {
    println!("agentnoise setup complete");
    println!("config: {}", result.config_path.display());
    println!(
        "identity: {}",
        if result.identity_created {
            "created"
        } else {
            "reused"
        }
    );
    println!("npub: {}", result.npub);
    println!("nprofile: {}", result.nprofile);
    if result.created_config {
        println!("config file: created");
    }
    if result.daemon_started {
        println!("daemon: started");
    }
    if result.login_repaired {
        println!("login: restored from OS keychain");
    }
    if result.profile_published {
        println!("profile: published");
    }
    if result.key_package_published {
        println!("key package: published");
    }
    if let Some(group_id) = &result.group_id {
        println!("group: {group_id}");
        println!();
        println!("next: agentnoise up");
    } else if let Some(output) = &result.group_output {
        println!("group: created, but the group id was not recognized in wn output");
        println!("{output}");
        println!();
        println!("next: agentnoise up --group <group-id>");
    } else {
        println!("relays:");
        for relay in &result.relays {
            println!("- {relay}");
        }
        println!();
        println!("{}", result.qr);
        println!("next: create a White Noise group with this desktop identity, then run:");
        println!("agentnoise up");
    }
}

fn service_command(config_path: &Path, args: ServiceArgs) -> Result<()> {
    match args.command {
        ServiceCommand::Print { target } => {
            let target = target.unwrap_or_else(service::default_target);
            let config = Config::load_or_template(config_path)?;
            let exe = std::env::current_exe().context("resolving current executable")?;
            println!("{}", service::render(target, &exe, config_path, &config));
        }
        ServiceCommand::Install {
            target,
            force,
            load,
            path,
        } => {
            let target = target.unwrap_or_else(service::default_target);
            let config = Config::load(config_path)?;
            let exe = std::env::current_exe().context("resolving current executable")?;
            let path = service::install(
                target,
                &exe,
                config_path,
                &config,
                force,
                load,
                path.as_deref(),
            )?;
            println!("installed {}", path.display());
        }
        ServiceCommand::Uninstall {
            target,
            unload,
            path,
        } => {
            let target = target.unwrap_or_else(service::default_target);
            match service::uninstall(target, unload, path.as_deref())? {
                Some(path) => println!("removed {}", path.display()),
                None => println!("not installed"),
            }
        }
    }
    Ok(())
}

fn up(config_path: &Path, args: UpArgs) -> Result<()> {
    let result = setup::setup(
        config_path,
        SetupOptions {
            phone_npub: args.phone.clone(),
            group_name: args.group_name,
            force_identity: false,
            relays: args.relays.clone(),
        },
    )?;

    let mut config = Config::load(config_path)?;
    if let Some(group) = args
        .group
        .as_deref()
        .map(str::trim)
        .filter(|group| !group.is_empty())
    {
        config.whitenoise.add_control_group_id(group);
        config.save(config_path)?;
    }

    if !config.whitenoise.has_control_group() {
        match discover_group(&mut config, config_path) {
            Err(error) => {
                print_setup_result(&result);
                eprintln!("agentnoise: group discovery failed: {error:#}");
                return Ok(());
            }
            Ok(GroupDiscovery::Ready) => {
                println!("agentnoise: discovered White Noise control chat(s)");
            }
            Ok(GroupDiscovery::NeedsPairing) => {
                print_setup_result(&result);
                println!();
                println!("next: scan the QR, create a White Noise chat with AgentNoise, then run:");
                println!("agentnoise up");
                return Ok(());
            }
        }
    }

    let groups = config.whitenoise.control_group_ids();
    println!("agentnoise ready");
    println!("npub: {}", result.npub);
    println!("groups: {}", groups.len());
    for group_id in groups.iter().take(5) {
        println!("- {group_id}");
    }
    if groups.len() > 5 {
        println!("- ...");
    }
    if let Some(first_repo) = config.default_repo_alias() {
        println!("default workspace: {first_repo}:/");
    }
    if config.whitenoise.allowed_senders.is_empty() && config.whitenoise.require_pairing_pin {
        println!("phone: scan the QR and send the desktop PIN as the first message");
    } else {
        println!("phone: send /help");
    }

    if args.no_listen {
        return Ok(());
    }

    start_listener(
        config_path,
        StartArgs {
            group: None,
            no_daemon: args.no_daemon,
        },
    )
}

enum GroupDiscovery {
    Ready,
    NeedsPairing,
}

fn discover_group(config: &mut Config, config_path: &Path) -> Result<GroupDiscovery> {
    let groups = whitenoise_cli::list_groups(&config.whitenoise)?;
    if groups.is_empty() {
        return Ok(GroupDiscovery::NeedsPairing);
    }

    for group in groups {
        config.whitenoise.add_control_group_id(&group.group_id);
    }
    config.save(config_path)?;
    Ok(GroupDiscovery::Ready)
}

fn start_listener(config_path: &Path, args: StartArgs) -> Result<()> {
    let mut config = Config::load(config_path)?;
    if let Some(group) = args
        .group
        .as_deref()
        .map(str::trim)
        .filter(|group| !group.is_empty())
    {
        config.whitenoise.add_control_group_id(group);
        config.save(config_path)?;
    }

    let _daemon = if args.no_daemon {
        None
    } else {
        let daemon = whitenoise_cli::ensure_daemon(&config.whitenoise)?;
        if daemon.is_some() {
            eprintln!("agentnoise: started White Noise daemon");
        }
        daemon
    };

    let pairing = pairing_for_listener(config_path, &config)?;
    run_listener(config_path, config, pairing)
}

fn pairing_for_listener(config_path: &Path, config: &Config) -> Result<Option<PairingRuntime>> {
    if !config.whitenoise.require_pairing_pin || !config.whitenoise.allowed_senders.is_empty() {
        return Ok(None);
    }

    let gate = PairingGate::new(config.whitenoise.pairing_pin_seconds);
    let payload = setup::pairing(config_path, &[])?;
    println!("agentnoise pairing required");
    println!("QR contains the desktop nprofile/npub and relay hints. It never contains the nsec.");
    println!("npub: {}", payload.npub);
    println!("nprofile: {}", payload.nprofile);
    println!();
    println!("{}", identity::render_qr(&payload.nprofile)?);
    println!();
    Ok(Some(PairingRuntime { gate, payload }))
}

fn run_listener(config_path: &Path, config: Config, pairing: Option<PairingRuntime>) -> Result<()> {
    if whitenoise_cli::ensure_login_from_keychain(&config.whitenoise)? {
        eprintln!("agentnoise: restored White Noise login from OS keychain");
    }
    if let Some(pairing) = pairing.clone() {
        spawn_pairing_pin_display(pairing);
    }
    let app = Arc::new(AgentNoise::new_with_auth(
        config_path.to_path_buf(),
        config,
        pairing.map(|pairing| pairing.gate),
    )?);
    let recovered = app.recover_interrupted_jobs()?;
    if recovered > 0 {
        eprintln!("agentnoise: marked {recovered} unfinished job(s) interrupted after restart");
    }
    let wn = Arc::new(WnClient::new(app.config().whitenoise.clone()));
    listen(config_path, app, wn)
}

fn spawn_pairing_pin_display(pairing: PairingRuntime) {
    thread::spawn(move || {
        let pairing_gate = pairing.gate;
        let payload = pairing.payload;
        while !pairing_gate.is_complete() {
            let pin = pairing_gate.current_pin();
            print_pairing_pin(pin.clone());
            let mut alert = match desktop_alert::spawn_pairing_pin_alert(
                &pin,
                &payload.npub,
                &payload.nprofile,
            ) {
                Ok(alert) => alert,
                Err(error) => {
                    eprintln!("agentnoise: failed to show pairing alert: {error:#}");
                    None
                }
            };
            let expires_after = Duration::from_secs(pin.expires_in_seconds.max(1));
            let started = Instant::now();
            while started.elapsed() < expires_after {
                if pairing_gate.is_complete() {
                    if let Some(alert) = alert.as_mut() {
                        alert.close();
                    }
                    show_pairing_success_alert();
                    return;
                }
                if alert
                    .as_mut()
                    .is_some_and(desktop_alert::AlertHandle::has_exited)
                {
                    alert = None;
                }
                thread::sleep(Duration::from_millis(500));
            }
            if let Some(alert) = alert.as_mut() {
                alert.close();
            }
        }
        show_pairing_success_alert();
    });
}

fn show_pairing_success_alert() {
    println!("agentnoise pairing complete");
    std::io::stdout().flush().ok();
    if let Err(error) = desktop_alert::show_pairing_success_alert() {
        eprintln!("agentnoise: failed to show pairing success alert: {error:#}");
    }
}

fn print_pairing_pin(pin: agentnoise::auth::PairingPin) {
    println!(
        "pairing PIN: {} (expires in {}s)",
        pin.code, pin.expires_in_seconds
    );
    println!("phone first message: {}", pin.code);
    std::io::stdout().flush().ok();
}

enum StreamItem {
    Event(agentnoise::wn::MessageEvent),
    StreamError {
        group_id: String,
        message: String,
    },
    Exited {
        group_id: String,
        status: ExitStatus,
    },
    Discovered(Vec<String>),
    DiscoveryError(String),
}

fn listen(config_path: &Path, app: Arc<AgentNoise>, wn: Arc<WnClient>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let subscribed = Arc::new(Mutex::new(HashSet::new()));

    let initial_groups = initial_group_ids(&wn);
    if initial_groups.is_empty() {
        bail!("no White Noise groups are configured or visible");
    }

    for group_id in initial_groups {
        subscribe_group_if_needed(
            Arc::clone(&wn),
            Arc::clone(&subscribed),
            tx.clone(),
            &group_id,
        )?;
    }
    spawn_group_discovery(Arc::clone(&wn), tx.clone());
    println!("agentnoise listening");

    let ignore_initial = app.config().whitenoise.ignore_initial_messages;
    let mut seen_ids = HashSet::new();

    for item in rx {
        match item {
            StreamItem::Discovered(group_ids) => {
                for group_id in group_ids {
                    if let Err(error) = subscribe_group_if_needed(
                        Arc::clone(&wn),
                        Arc::clone(&subscribed),
                        tx.clone(),
                        &group_id,
                    ) {
                        eprintln!("agentnoise: failed to subscribe to {group_id}: {error:#}");
                    }
                }
            }
            StreamItem::DiscoveryError(message) => {
                eprintln!("agentnoise: group discovery failed: {message}");
            }
            StreamItem::StreamError { group_id, message } => {
                eprintln!("agentnoise: wn stream error for {group_id}: {message}");
            }
            StreamItem::Exited { group_id, status } => {
                remove_subscribed_group(&subscribed, &group_id)?;
                if !status.success() {
                    eprintln!("agentnoise: wn subscribe for {group_id} exited with {status}");
                }
            }
            StreamItem::Event(event) => {
                let Some(group_id) = event.group_id.as_deref() else {
                    eprintln!("agentnoise: ignored message without White Noise group id");
                    continue;
                };
                if let Some(id) = &event.id {
                    let seen_key = format!("{group_id}:{id}");
                    if !seen_ids.insert(seen_key) {
                        continue;
                    }
                }
                let process_initial_pairing = event.unsupported.is_none()
                    && app.accepts_current_pairing_pin(event.sender.as_deref(), &event.text);
                if ignore_initial && event.is_initial && !process_initial_pairing {
                    continue;
                }

                if let Some(message) = event.unsupported.as_deref() {
                    match app.route_unsupported_message(event.sender.as_deref(), message)? {
                        RouteAction::Ignore => {}
                        RouteAction::Reply(reply) => {
                            wn.send_reply_to(group_id, &reply)?;
                        }
                        RouteAction::NewSession(_)
                        | RouteAction::ResumeSession(_)
                        | RouteAction::Run(_) => {}
                    }
                    continue;
                }

                match app.route_message(
                    event.group_id.as_deref(),
                    event.sender.as_deref(),
                    &event.text,
                )? {
                    RouteAction::Ignore => {}
                    RouteAction::Reply(reply) => {
                        wn.send_reply_to(group_id, &reply)?;
                    }
                    RouteAction::NewSession(request) => {
                        match create_parallel_session(
                            config_path,
                            Arc::clone(&app),
                            Arc::clone(&wn),
                            Arc::clone(&subscribed),
                            tx.clone(),
                            &request,
                        ) {
                            Ok(new_group_id) => {
                                match wn.send_reply_to(&new_group_id, &request.ready_text()) {
                                    Ok(()) => {
                                        wn.send_reply_to(group_id, &request.created_text())?
                                    }
                                    Err(error) => {
                                        wn.send_reply_to(
                                            group_id,
                                            &format!(
                                                "{}\nWarning: failed to send the ready message to the new chat: {error:#}",
                                                request.created_text()
                                            ),
                                        )?;
                                    }
                                }
                            }
                            Err(error) => {
                                wn.send_reply_to(
                                    group_id,
                                    &format!("Error: failed to create session: {error:#}"),
                                )?;
                            }
                        }
                    }
                    RouteAction::ResumeSession(request) => {
                        match resume_parallel_session(
                            config_path,
                            Arc::clone(&wn),
                            Arc::clone(&subscribed),
                            tx.clone(),
                            &request,
                        ) {
                            Ok(()) => {
                                if request.group_id == group_id {
                                    wn.send_reply_to(group_id, &request.target_text)?;
                                } else {
                                    match wn.send_reply_to(&request.group_id, &request.target_text)
                                    {
                                        Ok(()) => {
                                            wn.send_reply_to(group_id, &request.reply_text)?
                                        }
                                        Err(error) => {
                                            wn.send_reply_to(
                                                group_id,
                                                &format!(
                                                    "Error: resumed session locally, but failed to message the target chat: {error:#}"
                                                ),
                                            )?;
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                wn.send_reply_to(
                                    group_id,
                                    &format!("Error: failed to resume session: {error:#}"),
                                )?;
                            }
                        }
                    }
                    RouteAction::Run(request) => {
                        wn.send_reply_to(group_id, "Job accepted.")?;
                        let app = Arc::clone(&app);
                        let wn = Arc::clone(&wn);
                        let group_id = group_id.to_string();
                        std::thread::spawn(move || {
                            let reply = match app.run_request(request) {
                                Ok(reply) => reply,
                                Err(error) => {
                                    format!("Error: job failed to start: {error:#}")
                                }
                            };
                            if let Err(error) = wn.send_reply_to(&group_id, &reply) {
                                eprintln!("agentnoise: failed to send job reply: {error:#}");
                            }
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

fn create_parallel_session(
    config_path: &Path,
    app: Arc<AgentNoise>,
    wn: Arc<WnClient>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<StreamItem>,
    request: &NewSessionRequest,
) -> Result<String> {
    let created = whitenoise_cli::create_group(
        &app.config().whitenoise,
        &request.group_name(),
        std::slice::from_ref(&request.sender),
    )?;
    let group_id = created
        .group_id
        .context("White Noise did not return the new group id")?;

    app.create_session_record(&group_id, request.state.clone())?;
    persist_control_group_id(config_path, &group_id)?;
    subscribe_group_if_needed(wn, subscribed, tx, &group_id)?;

    Ok(group_id)
}

fn resume_parallel_session(
    config_path: &Path,
    wn: Arc<WnClient>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<StreamItem>,
    request: &agentnoise::app::ResumeSessionRequest,
) -> Result<()> {
    persist_control_group_id(config_path, &request.group_id)?;
    subscribe_group_if_needed(wn, subscribed, tx, &request.group_id)
}

fn persist_control_group_id(config_path: &Path, group_id: &str) -> Result<()> {
    let mut config = Config::load(config_path)?;
    config.whitenoise.add_control_group_id(group_id);
    config.save(config_path)
}

fn initial_group_ids(wn: &WnClient) -> Vec<String> {
    let mut group_ids = wn.configured_group_ids();
    match wn.discover_group_ids() {
        Ok(discovered) => extend_unique(&mut group_ids, discovered),
        Err(error) => eprintln!("agentnoise: group discovery failed: {error:#}"),
    }
    group_ids
}

fn subscribe_group_if_needed(
    wn: Arc<WnClient>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<StreamItem>,
    group_id: &str,
) -> Result<()> {
    let group_id = group_id.trim();
    if group_id.is_empty() {
        return Ok(());
    }

    {
        let subscribed = subscribed
            .lock()
            .map_err(|_| anyhow::anyhow!("subscribed group lock poisoned"))?;
        if subscribed.contains(group_id) {
            return Ok(());
        }
    }

    let group_id = group_id.to_string();
    let mut child = wn
        .subscribe_group(&group_id)
        .with_context(|| format!("starting White Noise subscription for {group_id}"))?;
    let stdout = child
        .stdout
        .take()
        .context("wn subscribe did not expose stdout")?;
    {
        let mut subscribed = subscribed
            .lock()
            .map_err(|_| anyhow::anyhow!("subscribed group lock poisoned"))?;
        subscribed.insert(group_id.clone());
    }
    println!("agentnoise listening group {group_id}");
    spawn_group_subscription(group_id, child, stdout, tx);
    Ok(())
}

fn spawn_group_subscription(
    group_id: String,
    mut child: Child,
    stdout: ChildStdout,
    tx: mpsc::Sender<StreamItem>,
) {
    thread::spawn(move || {
        for value in WnClient::parse_events_from_reader(stdout) {
            let value = match value {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(StreamItem::StreamError {
                        group_id: group_id.clone(),
                        message: format!("{error:#}"),
                    });
                    continue;
                }
            };
            if let Some(error) = WnClient::error_message(&value) {
                let _ = tx.send(StreamItem::StreamError {
                    group_id: group_id.clone(),
                    message: error,
                });
                continue;
            }

            for event in WnClient::parse_events_for_group(&value, &group_id) {
                if tx.send(StreamItem::Event(event)).is_err() {
                    return;
                }
            }
        }

        match child.wait() {
            Ok(status) => {
                let _ = tx.send(StreamItem::Exited { group_id, status });
            }
            Err(error) => {
                let _ = tx.send(StreamItem::StreamError {
                    group_id,
                    message: format!("waiting for wn subscribe failed: {error:#}"),
                });
            }
        }
    });
}

fn spawn_group_discovery(wn: Arc<WnClient>, tx: mpsc::Sender<StreamItem>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(30));
            match wn.discover_group_ids() {
                Ok(group_ids) => {
                    if !group_ids.is_empty() && tx.send(StreamItem::Discovered(group_ids)).is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    if tx
                        .send(StreamItem::DiscoveryError(format!("{error:#}")))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

fn remove_subscribed_group(subscribed: &Arc<Mutex<HashSet<String>>>, group_id: &str) -> Result<()> {
    subscribed
        .lock()
        .map_err(|_| anyhow::anyhow!("subscribed group lock poisoned"))?
        .remove(group_id);
    Ok(())
}

fn extend_unique(group_ids: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for group_id in more {
        let group_id = group_id.trim();
        if !group_id.is_empty() && !group_ids.iter().any(|existing| existing == group_id) {
            group_ids.push(group_id.to_string());
        }
    }
}
