use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, ExitStatus};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use agentnoise::app::{AgentApp, NewSessionRequest, RouteAction};
use agentnoise::auth::PairingGate;
use agentnoise::config::Config;
use agentnoise::desktop_alert;
use agentnoise::doctor::render_doctor;
use agentnoise::events::EventJournal;
use agentnoise::identity;
use agentnoise::launchd;
use agentnoise::runner::{AgentKind, AgentRequest};
use agentnoise::runtime::{self, AcquireMode, EngineGuard, RuntimePairingInfo};
use agentnoise::secrets;
use agentnoise::service::{self, ServiceTarget};
use agentnoise::setup::{self, SetupOptions, SetupResult};
use agentnoise::whitenoise_cli::{self, WhitenoiseInstall};
use agentnoise::wn::WnClient;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use zeroize::Zeroize;

const FIRST_PAIRING_SUBSCRIBE_LIMIT: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerMode {
    Try,
    Wait,
    AttachIfBusy,
}

#[derive(Clone)]
struct PairingRuntime {
    gate: PairingGate,
    payload: identity::PairingPayload,
    display: PairingDisplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairingDisplay {
    Desktop,
    TerminalOnly,
}

#[derive(Debug, Parser)]
#[command(name = "agentnoise")]
#[command(about = "Chat with local coding agents through White Noise")]
#[command(version)]
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
    #[command(about = "Set up agentnoise, discover control chats, and listen")]
    Up(UpArgs),
    #[command(about = "Show the phone pairing QR for the desktop identity")]
    Pair(PairArgs),
    #[command(about = "Show runtime status and diagnostics")]
    Status(StatusArgs),
    Doctor(DoctorArgs),
    #[command(about = "List configured coding-agent capabilities")]
    Agents(AgentsArgs),
    #[command(about = "Run an isolated fake White Noise phone for local testing")]
    FakePhone(FakePhoneArgs),
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
    #[command(about = "Create and pair agentnoise identities")]
    Identity(IdentityArgs),
    #[command(about = "Manage the agentnoise OS keychain bootstrap secret")]
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
    #[arg(long, help = "Unique White Noise/Nostr profile name for this machine")]
    name: Option<String>,
    #[arg(long, default_value = setup::DEFAULT_GROUP_NAME)]
    group_name: String,
    #[arg(long)]
    force_identity: bool,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(
        long,
        help = "Development only: use a plaintext throwaway nsec under the agentnoise data dir instead of the OS keychain"
    )]
    dev_burner_nsec: bool,
}

#[derive(Debug, Args)]
struct UpArgs {
    #[arg(
        long,
        help = "Phone White Noise npub; creates a control chat when provided"
    )]
    phone: Option<String>,
    #[arg(long, help = "Unique White Noise/Nostr profile name for this machine")]
    name: Option<String>,
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
    #[arg(
        long,
        help = "Development only: use a plaintext throwaway nsec under the agentnoise data dir instead of the OS keychain"
    )]
    dev_burner_nsec: bool,
    #[arg(
        long,
        help = "SSH setup mode: show PIN only in this terminal, not a desktop alert"
    )]
    ssh: bool,
}

#[derive(Debug, Args)]
struct PairArgs {
    #[arg(long = "relay")]
    relays: Vec<String>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct AgentsArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct FakePhoneArgs {
    #[command(subcommand)]
    command: FakePhoneCommand,
}

#[derive(Debug, Args)]
struct StartArgs {
    #[arg(long, help = "Add a White Noise group id before starting")]
    group: Option<String>,
    #[arg(long, help = "Do not start wn daemon automatically")]
    no_daemon: bool,
    #[arg(
        long,
        help = "SSH setup mode: show PIN only in this terminal, not a desktop alert"
    )]
    ssh: bool,
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
    #[command(about = "List White Noise account relays used for message delivery")]
    Relays,
    #[command(about = "Add configured message relays to the White Noise account")]
    EnsureRelays,
    #[command(about = "Print the resolved wn path")]
    Path,
    #[command(about = "Install wn and wnd under agentnoise's managed data directory")]
    Install {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    #[command(about = "Show wn daemon status")]
    DaemonStatus,
    #[command(about = "Run wn login using the configured bootstrap nsec")]
    LoginFromKeychain {
        #[arg(long)]
        relay: Option<String>,
    },
    #[command(about = "Send one raw JSON-line request to wnd's Unix socket")]
    SocketProbe {
        #[arg(long, default_value = "ping")]
        method: String,
    },
}

#[derive(Debug, Subcommand)]
enum FakePhoneCommand {
    #[command(about = "Print the isolated fake phone paths")]
    Plan {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Create a fake phone identity, chat, and send a test message")]
    Roundtrip {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        pin: Option<String>,
        #[arg(long, default_value = "agentnoise fake phone")]
        group_name: String,
        #[arg(long, default_value_t = 60)]
        timeout_seconds: u64,
        #[arg(required = true, trailing_var_arg = true)]
        message: Vec<String>,
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
    #[command(about = "Show the configured desktop identity and published profile labels")]
    Status,
    #[command(about = "Change the configured White Noise/Nostr profile name for this machine")]
    Rename {
        name: String,
        #[arg(long, help = "Save config only; publish on the next setup/up run")]
        no_publish: bool,
    },
    #[command(
        about = "Generate one or more Nostr identities and store nsecs in the configured identity store"
    )]
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
    #[command(about = "Delete a named identity nsec from the configured identity store")]
    Delete {
        #[arg(long, default_value = "desktop")]
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum KeychainCommand {
    #[command(about = "Store a White Noise nsec in the OS keychain")]
    StoreNsec,
    #[command(about = "Check whether the OS keychain contains an agentnoise nsec")]
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
    let cli = Cli::parse_from(normalized_cli_args());
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
                    profile_name: args.name,
                    group_name: args.group_name,
                    force_identity: args.force_identity,
                    relays: args.relays,
                    dev_burner_nsec: args.dev_burner_nsec,
                    start_daemon: true,
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
        Command::Status(args) => {
            let config = Config::load_or_template(&config_path)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&agentnoise::diagnostics::status_report(
                        &config
                    )?)?
                );
            } else {
                println!(
                    "{}",
                    agentnoise::diagnostics::render_status_report(&config_path, &config)
                );
            }
        }
        Command::Doctor(args) => {
            let config = Config::load_or_template(&config_path)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&agentnoise::diagnostics::status_report(
                        &config
                    )?)?
                );
            } else {
                println!("{}", render_doctor(&config_path, &config));
            }
        }
        Command::Agents(args) => {
            let config = Config::load_or_template(&config_path)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&agentnoise::capabilities::capabilities(&config))?
                );
            } else {
                println!("{}", agentnoise::capabilities::render_capabilities(&config));
            }
        }
        Command::FakePhone(args) => {
            fake_phone_command(&config_path, args)?;
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
            let app = AgentApp::from_config_path(&config_path)?;
            match app.route_message(group.as_deref(), sender.as_deref(), &message)? {
                RouteAction::Ignore => {}
                RouteAction::Reply(reply) => println!("{reply}"),
                RouteAction::NewSession(request) => {
                    println!("New session requested: {}", request.name);
                    println!(
                        "Run this command from the live listener so agentnoise can create the White Noise chat."
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
            let app = AgentApp::from_config_path(&config_path)?;
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                bail!("prompt cannot be empty");
            }
            let request = AgentRequest::new(agent, repo, prompt);
            println!("{}", app.run_request(request)?);
        }
        Command::Start(args) => {
            start_listener(&config_path, args, ListenerMode::Try)?;
        }
        Command::Listen => {
            let config = Config::load(&config_path)?;
            let pairing = pairing_for_listener(&config_path, &config, PairingDisplay::Desktop)?;
            run_listener_with_mode(&config_path, config, pairing, ListenerMode::Try)?;
        }
        Command::Send { text } => {
            let config = Config::load(&config_path)?;
            if whitenoise_cli::ensure_login_from_configured_nsec(&config.whitenoise)? {
                eprintln!("agentnoise: restored White Noise login from configured nsec");
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
                WhitenoiseCommand::Relays => {
                    print_whitenoise_relays(&config)?;
                }
                WhitenoiseCommand::EnsureRelays => {
                    if whitenoise_cli::ensure_login_from_configured_nsec(&config.whitenoise)? {
                        eprintln!("agentnoise: restored White Noise login from configured nsec");
                    }
                    let summary = whitenoise_cli::ensure_message_relays(&config.whitenoise)?;
                    println!("configured relays: {}", summary.configured_relays);
                    println!("added relay entries: {}", summary.added_entries);
                    println!(
                        "already present entries: {}",
                        summary.already_present_entries
                    );
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
                    let output = whitenoise_cli::login_from_configured_nsec(
                        &config.whitenoise,
                        relay.as_deref(),
                    )?;
                    if output.is_empty() {
                        println!("logged in from configured nsec");
                    } else {
                        println!("{output}");
                    }
                }
                WhitenoiseCommand::SocketProbe { method } => {
                    let Some(socket) = config.whitenoise.resolved_socket() else {
                        bail!("whitenoise.socket is not configured");
                    };
                    println!(
                        "{}",
                        agentnoise::wnd_socket::render_socket_probe(&socket, &method)
                    );
                }
            }
        }
        Command::Identity(args) => {
            let mut config = Config::load_or_template(&config_path)?;
            match args.command {
                IdentityCommand::Status => {
                    print_identity_status(&config);
                }
                IdentityCommand::Rename { name, no_publish } => {
                    rename_identity_profile(&config_path, &mut config, &name, no_publish)?;
                }
                IdentityCommand::Create { name, count, force } => {
                    let identities =
                        identity::create_identities(&config.whitenoise, &name, count, force)?;
                    println!("stored agentnoise identity nsecs");
                    for identity in identities {
                        println!();
                        println!("name: {}", identity.name);
                        println!("npub: {}", identity.npub);
                        println!(
                            "store: {}",
                            identity::identity_secret_label(&config.whitenoise, &identity.name)
                        );
                    }
                }
                IdentityCommand::Show { name } => {
                    let public = identity::load_public_identity(&config.whitenoise, &name)?;
                    println!("name: {}", public.name);
                    println!("npub: {}", public.npub);
                    println!(
                        "store: {}",
                        identity::identity_secret_label(&config.whitenoise, &public.name)
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
                    let label = identity::delete_identity_nsec(&config.whitenoise, &name)?;
                    println!("deleted agentnoise identity nsec from {label}");
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

fn normalized_cli_args() -> Vec<String> {
    let mut args = std::env::args().collect::<Vec<_>>();
    if args.len() >= 3 && args[1] == "--" {
        args.remove(1);
    }
    args
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
    println!("profile: {}", result.profile_display_name);
    if result.created_config {
        println!("config file: created");
    }
    if result.daemon_started {
        println!("daemon: started");
    }
    if result.login_repaired {
        println!("login: restored from configured nsec");
    }
    if result.profile_published {
        println!("profile: published");
    }
    if result.key_package_published {
        println!("key package: published");
    }
    if result.message_relay_entries_added > 0 {
        println!(
            "message relay entries added: {}",
            result.message_relay_entries_added
        );
    }
    if let Some(path) = &result.dev_burner_nsec_file {
        println!("dev burner nsec: {}", path.display());
        println!("warning: development-only plaintext secret; do not use for a real identity");
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
        println!("next: create a White Noise group with this desktop identity.");
        println!("If agentnoise is already running, it will discover the chat automatically.");
        println!("Otherwise run: agentnoise up");
    }
}

fn print_identity_status(config: &Config) {
    println!("identity: {}", identity::DEFAULT_IDENTITY_NAME);
    println!("profile name: {}", config.whitenoise.profile_name);
    println!(
        "profile display: {}",
        config.whitenoise.profile_display_name
    );
    println!("profile about: {}", config.whitenoise.profile_about);
    println!(
        "store: {}",
        identity::identity_secret_label(&config.whitenoise, identity::DEFAULT_IDENTITY_NAME)
    );
    if let Some(npub) = config
        .whitenoise
        .account
        .as_deref()
        .or(config.whitenoise.bot_npub.as_deref())
    {
        println!("npub: {npub}");
    } else {
        println!("npub: unavailable; run `agentnoise up` once to create the desktop identity");
    }
    let groups = config.whitenoise.control_group_ids();
    println!("groups: {}", groups.len());
    println!(
        "allowed senders: {}",
        config.whitenoise.allowed_senders.len()
    );
    println!("pairing relays:");
    for relay in identity::pairing_relays(&config.whitenoise, &[]) {
        println!("- {relay}");
    }
    println!("message relays:");
    for relay in &config.whitenoise.message_relays {
        println!("- {relay}");
    }
}

fn rename_identity_profile(
    config_path: &Path,
    config: &mut Config,
    name: &str,
    no_publish: bool,
) -> Result<()> {
    let display_name = name.trim();
    if display_name.is_empty() {
        bail!("profile name cannot be empty");
    }

    config.whitenoise.profile_name = setup::normalize_profile_name(display_name);
    config.whitenoise.profile_display_name = display_name.to_string();
    config.save(config_path)?;

    println!("profile name: {}", config.whitenoise.profile_name);
    println!(
        "profile display: {}",
        config.whitenoise.profile_display_name
    );
    if no_publish {
        println!("profile: saved; next agentnoise setup/up publishes it");
        return Ok(());
    }

    if config.whitenoise.account.is_none()
        && let Ok(public) =
            identity::load_public_identity(&config.whitenoise, identity::DEFAULT_IDENTITY_NAME)
    {
        config.whitenoise.account = Some(public.npub.clone());
        config.whitenoise.bot_npub = Some(public.npub);
        config.save(config_path)?;
    }

    match publish_configured_profile(config) {
        Ok(()) => println!("profile: published"),
        Err(error) => {
            println!("profile: saved; publish failed: {error:#}");
            println!("run `agentnoise up` after fixing White Noise login to publish it");
        }
    }

    Ok(())
}

fn publish_configured_profile(config: &Config) -> Result<()> {
    let _daemon = whitenoise_cli::ensure_daemon(&config.whitenoise)?;
    if whitenoise_cli::ensure_login_from_configured_nsec(&config.whitenoise)? {
        eprintln!("agentnoise: restored White Noise login from configured nsec");
    }
    whitenoise_cli::update_profile(
        &config.whitenoise,
        &config.whitenoise.profile_name,
        &config.whitenoise.profile_display_name,
        &config.whitenoise.profile_about,
    )?;
    Ok(())
}

fn print_whitenoise_relays(config: &Config) -> Result<()> {
    let relays = whitenoise_cli::list_relays(&config.whitenoise)?;
    println!("configured message relays:");
    for relay in &config.whitenoise.message_relays {
        println!("- {relay}");
    }
    println!("White Noise account relays:");
    if relays.is_empty() {
        println!("- none");
    } else {
        for relay in relays {
            let types = if relay.types.is_empty() {
                "-".to_string()
            } else {
                relay.types.join(",")
            };
            let status = relay.status.unwrap_or_else(|| "unknown".to_string());
            println!("- {} [{}] {}", relay.url, types, status);
        }
    }
    Ok(())
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
    if should_attach_before_setup(config_path, &args)? {
        let config = Config::load(config_path)?;
        runtime::attach_ui(config_path, &config)?;
        return Ok(());
    }
    wait_before_setup_if_needed(config_path, &args)?;

    let result = setup::setup(
        config_path,
        SetupOptions {
            phone_npub: args.phone.clone(),
            profile_name: args.name.clone(),
            group_name: args.group_name,
            force_identity: false,
            relays: args.relays.clone(),
            dev_burner_nsec: args.dev_burner_nsec,
            start_daemon: !args.no_daemon,
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
                if args.no_listen {
                    println!(
                        "next: scan the QR, create a White Noise chat with agentnoise, then run:"
                    );
                    println!("agentnoise up");
                    return Ok(());
                }
                println!("agentnoise: waiting for a White Noise control chat");
                println!("agentnoise: scan the QR, create the chat, then send the pairing PIN");
            }
        }
    }

    let groups = config.whitenoise.control_group_ids();
    if groups.is_empty() {
        println!("agentnoise ready for first pairing");
    } else {
        println!("agentnoise ready");
    }
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
            ssh: args.ssh,
        },
        if args.ssh {
            ListenerMode::Try
        } else {
            up_listener_mode()
        },
    )
}

fn should_attach_before_setup(config_path: &Path, args: &UpArgs) -> Result<bool> {
    if !runtime::stdio_is_interactive()
        || args.no_listen
        || args.phone.is_some()
        || args.group.is_some()
        || args.name.is_some()
        || !args.relays.is_empty()
        || args.dev_burner_nsec
        || args.ssh
        || !config_path.exists()
    {
        return Ok(false);
    }

    let config = Config::load(config_path)?;
    runtime::engine_is_running(&config)
}

fn fake_phone_command(config_path: &Path, args: FakePhoneArgs) -> Result<()> {
    let config = Config::load(config_path)?;
    match args.command {
        FakePhoneCommand::Plan { root, json } => {
            let plan = agentnoise::fake_phone::plan(&config, root.as_deref());
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                println!("fake phone");
                println!("root: {}", plan.root.display());
                println!("data: {}", plan.data_dir.display());
                println!("logs: {}", plan.logs_dir.display());
                println!("socket: {}", plan.socket.display());
                println!("nsec: {}", plan.nsec_file.display());
            }
        }
        FakePhoneCommand::Roundtrip {
            root,
            pin,
            group_name,
            timeout_seconds,
            message,
        } => {
            let message = message.join(" ");
            if message.trim().is_empty() {
                bail!("message cannot be empty");
            }
            let root = root.unwrap_or_else(|| config.resolved_data_dir().join("fake-phone"));
            let result = agentnoise::fake_phone::roundtrip(
                &config,
                agentnoise::fake_phone::FakePhoneRoundtrip {
                    root,
                    pin,
                    message,
                    group_name,
                    timeout: Duration::from_secs(timeout_seconds.max(1)),
                },
            )?;
            println!("fake phone npub: {}", result.phone_npub);
            println!("group: {}", result.group_id);
            if result.replies.is_empty() {
                println!("replies: none before timeout");
            } else {
                println!("replies:");
                for reply in result.replies {
                    println!("- {reply}");
                }
            }
        }
    }
    Ok(())
}

fn wait_before_setup_if_needed(config_path: &Path, args: &UpArgs) -> Result<()> {
    if runtime::stdio_is_interactive()
        || args.no_listen
        || args.phone.is_some()
        || args.group.is_some()
        || args.name.is_some()
        || !args.relays.is_empty()
        || args.dev_burner_nsec
        || args.ssh
        || !config_path.exists()
    {
        return Ok(());
    }

    let config = Config::load(config_path)?;
    while runtime::engine_is_running(&config)? {
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn up_listener_mode() -> ListenerMode {
    if runtime::stdio_is_interactive() {
        ListenerMode::AttachIfBusy
    } else {
        ListenerMode::Wait
    }
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

fn start_listener(config_path: &Path, args: StartArgs, mode: ListenerMode) -> Result<()> {
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

    let guard = acquire_listener_guard(config_path, &config, mode)?;
    let Some(guard) = guard else {
        runtime::attach_ui(config_path, &config)?;
        return Ok(());
    };

    let _daemon = if args.no_daemon {
        None
    } else {
        let daemon = whitenoise_cli::ensure_daemon(&config.whitenoise)?;
        if daemon.is_some() {
            eprintln!("agentnoise: started White Noise daemon");
        }
        daemon
    };

    let pairing_display = if args.ssh {
        PairingDisplay::TerminalOnly
    } else {
        PairingDisplay::Desktop
    };
    let pairing = pairing_for_listener(config_path, &config, pairing_display)?;
    if let Some(pairing) = pairing_runtime_info(&config, pairing.as_ref()) {
        guard.update_status(config_path, &config, Some(pairing))?;
    }
    run_listener(config_path, config, pairing, guard)
}

fn pairing_for_listener(
    config_path: &Path,
    config: &Config,
    display: PairingDisplay,
) -> Result<Option<PairingRuntime>> {
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
    Ok(Some(PairingRuntime {
        gate,
        payload,
        display,
    }))
}

fn run_listener_with_mode(
    config_path: &Path,
    config: Config,
    pairing: Option<PairingRuntime>,
    mode: ListenerMode,
) -> Result<()> {
    let guard = acquire_listener_guard(config_path, &config, mode)?;
    let Some(guard) = guard else {
        runtime::attach_ui(config_path, &config)?;
        return Ok(());
    };
    if let Some(pairing) = pairing_runtime_info(&config, pairing.as_ref()) {
        guard.update_status(config_path, &config, Some(pairing))?;
    }
    run_listener(config_path, config, pairing, guard)
}

fn acquire_listener_guard(
    config_path: &Path,
    config: &Config,
    mode: ListenerMode,
) -> Result<Option<EngineGuard>> {
    match mode {
        ListenerMode::Try => runtime::acquire_engine(config_path, config, AcquireMode::Try)?
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!("agentnoise is already running; use `agentnoise up` to attach")
            }),
        ListenerMode::Wait => runtime::acquire_engine(config_path, config, AcquireMode::Wait),
        ListenerMode::AttachIfBusy => {
            runtime::acquire_engine(config_path, config, AcquireMode::Try)
        }
    }
}

fn pairing_runtime_info(
    config: &Config,
    pairing: Option<&PairingRuntime>,
) -> Option<RuntimePairingInfo> {
    pairing.map(|pairing| RuntimePairingInfo {
        npub: pairing.payload.npub.clone(),
        nprofile: pairing.payload.nprofile.clone(),
        relays: pairing.payload.relays.clone(),
        pin_seconds: config.whitenoise.pairing_pin_seconds,
    })
}

fn run_listener(
    config_path: &Path,
    config: Config,
    pairing: Option<PairingRuntime>,
    _guard: EngineGuard,
) -> Result<()> {
    if whitenoise_cli::ensure_login_from_configured_nsec(&config.whitenoise)? {
        eprintln!("agentnoise: restored White Noise login from configured nsec");
    }
    match whitenoise_cli::ensure_message_relays(&config.whitenoise) {
        Ok(summary) if summary.added_entries > 0 => {
            eprintln!(
                "agentnoise: added {} White Noise message relay entries",
                summary.added_entries
            );
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("agentnoise: failed to ensure White Noise message relays: {error:#}");
        }
    }
    if let Some(pairing) = pairing.clone() {
        spawn_pairing_pin_display(pairing);
    }
    let app = Arc::new(AgentApp::new_with_auth(
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
        let display = pairing.display;
        if display == PairingDisplay::TerminalOnly {
            println!("agentnoise SSH pairing mode");
            println!("desktop alerts disabled; keep this terminal open until pairing completes");
            std::io::stdout().flush().ok();
        }
        while !pairing_gate.is_complete() {
            let pin = pairing_gate.current_pin();
            print_pairing_pin(pin.clone());
            let mut alert = match display {
                PairingDisplay::Desktop => {
                    match desktop_alert::spawn_pairing_pin_alert(
                        &pin,
                        &payload.npub,
                        &payload.nprofile,
                    ) {
                        Ok(alert) => alert,
                        Err(error) => {
                            eprintln!("agentnoise: failed to show pairing alert: {error:#}");
                            None
                        }
                    }
                }
                PairingDisplay::TerminalOnly => None,
            };
            let expires_after = Duration::from_secs(pin.expires_in_seconds.max(1));
            let started = Instant::now();
            while started.elapsed() < expires_after {
                if pairing_gate.is_complete() {
                    if let Some(alert) = alert.as_mut() {
                        alert.close();
                    }
                    show_pairing_success(display);
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
        show_pairing_success(display);
    });
}

fn show_pairing_success(display: PairingDisplay) {
    match display {
        PairingDisplay::Desktop => show_pairing_success_alert(),
        PairingDisplay::TerminalOnly => {
            println!("agentnoise pairing complete");
            std::io::stdout().flush().ok();
        }
    }
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

fn listen(config_path: &Path, app: Arc<AgentApp>, wn: Arc<WnClient>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let subscribed = Arc::new(Mutex::new(HashSet::new()));
    let event_journal = Arc::new(Mutex::new(EventJournal::open(
        &app.config().resolved_event_log_path(),
    )?));

    let initial_groups = initial_group_ids(&wn);
    let subscribe_limit = listener_subscribe_limit(app.config());
    if initial_groups.is_empty() {
        println!("agentnoise waiting for White Noise control chat");
        println!("agentnoise will keep discovering chats until the phone-created chat appears");
    } else {
        for group_id in initial_groups {
            subscribe_group_if_needed(
                Arc::clone(&wn),
                Arc::clone(&subscribed),
                tx.clone(),
                &group_id,
                subscribe_limit,
            )?;
        }
    }
    spawn_group_discovery(Arc::clone(&wn), tx.clone());
    println!("agentnoise listening");

    let ignore_initial = app.config().whitenoise.ignore_initial_messages;

    for item in rx {
        match item {
            StreamItem::Discovered(group_ids) => {
                for group_id in group_ids {
                    if let Err(error) = persist_control_group_id(config_path, &group_id) {
                        eprintln!(
                            "agentnoise: failed to persist discovered group {group_id}: {error:#}"
                        );
                    }
                    if let Err(error) = subscribe_group_if_needed(
                        Arc::clone(&wn),
                        Arc::clone(&subscribed),
                        tx.clone(),
                        &group_id,
                        subscribe_limit,
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
                {
                    let mut journal = event_journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("event journal lock poisoned"))?;
                    if journal.already_seen(group_id, event.id.as_deref()) {
                        continue;
                    }
                    if let Err(error) = journal.record_inbound(&event) {
                        eprintln!("agentnoise: failed to record inbound event: {error:#}");
                    }
                }
                let process_initial_pairing = event.unsupported.is_none()
                    && app.accepts_current_pairing_pin(event.sender.as_deref(), &event.text);
                if ignore_initial && event.is_initial && !process_initial_pairing {
                    match app.route_initial_history_event(&event)? {
                        RouteAction::Ignore => {}
                        RouteAction::Reply(reply) => {
                            send_reply_recorded(&wn, &event_journal, group_id, &reply)?;
                        }
                        RouteAction::NewSession(_)
                        | RouteAction::ResumeSession(_)
                        | RouteAction::Run(_) => {}
                    }
                    continue;
                }

                if let Some(message) = event.unsupported.as_deref() {
                    let action = if event.attachments.is_empty() {
                        app.route_unsupported_message(event.sender.as_deref(), message)?
                    } else {
                        app.route_unsupported_event(&event)?
                    };
                    match action {
                        RouteAction::Ignore => {}
                        RouteAction::Reply(reply) => {
                            send_reply_recorded(&wn, &event_journal, group_id, &reply)?;
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
                        send_reply_recorded(&wn, &event_journal, group_id, &reply)?;
                    }
                    RouteAction::NewSession(request) => {
                        match create_parallel_session(
                            config_path,
                            Arc::clone(&app),
                            Arc::clone(&wn),
                            Arc::clone(&subscribed),
                            tx.clone(),
                            &request,
                            subscribe_limit,
                        ) {
                            Ok(new_group_id) => {
                                match send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    &new_group_id,
                                    &request.ready_text(),
                                ) {
                                    Ok(()) => send_reply_recorded(
                                        &wn,
                                        &event_journal,
                                        group_id,
                                        &request.created_text(),
                                    )?,
                                    Err(error) => {
                                        send_reply_recorded(
                                            &wn,
                                            &event_journal,
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
                                send_reply_recorded(
                                    &wn,
                                    &event_journal,
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
                            subscribe_limit,
                        ) {
                            Ok(()) => {
                                if request.group_id == group_id {
                                    send_reply_recorded(
                                        &wn,
                                        &event_journal,
                                        group_id,
                                        &request.target_text,
                                    )?;
                                } else {
                                    match send_reply_recorded(
                                        &wn,
                                        &event_journal,
                                        &request.group_id,
                                        &request.target_text,
                                    ) {
                                        Ok(()) => send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            group_id,
                                            &request.reply_text,
                                        )?,
                                        Err(error) => {
                                            send_reply_recorded(
                                                &wn,
                                                &event_journal,
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
                                send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &format!("Error: failed to resume session: {error:#}"),
                                )?;
                            }
                        }
                    }
                    RouteAction::Run(request) => {
                        send_reply_recorded(&wn, &event_journal, group_id, "Job accepted.")?;
                        let app = Arc::clone(&app);
                        let wn = Arc::clone(&wn);
                        let event_journal = Arc::clone(&event_journal);
                        let group_id = group_id.to_string();
                        std::thread::spawn(move || {
                            let progress_wn = Arc::clone(&wn);
                            let progress_journal = Arc::clone(&event_journal);
                            let progress_group = group_id.clone();
                            let reply = match app.run_request_with_progress(request, move |text| {
                                if let Err(error) = send_reply_recorded(
                                    &progress_wn,
                                    &progress_journal,
                                    &progress_group,
                                    &text,
                                ) {
                                    eprintln!(
                                        "agentnoise: failed to send progress reply: {error:#}"
                                    );
                                }
                            }) {
                                Ok(reply) => reply,
                                Err(error) => {
                                    format!("Error: job failed to start: {error:#}")
                                }
                            };
                            if let Err(error) =
                                send_reply_recorded(&wn, &event_journal, &group_id, &reply)
                            {
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

fn send_reply_recorded(
    wn: &WnClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    text: &str,
) -> Result<()> {
    const ATTEMPTS: usize = 3;

    let mut last_error = None;
    for attempt in 1..=ATTEMPTS {
        match wn.send_reply_to(group_id, text) {
            Ok(()) => {
                let detail = (attempt > 1).then(|| format!("sent after {attempt} attempts"));
                if let Ok(mut journal) = event_journal.lock()
                    && let Err(error) = journal.record_outbound(group_id, text, true, detail)
                {
                    eprintln!("agentnoise: failed to record outbound event: {error:#}");
                }
                return Ok(());
            }
            Err(error) => {
                let detail = format!("{error:#}");
                if attempt < ATTEMPTS {
                    eprintln!(
                        "agentnoise: send reply failed, retrying ({attempt}/{ATTEMPTS}): {detail}"
                    );
                    thread::sleep(Duration::from_millis(500 * attempt as u64));
                }
                last_error = Some(detail);
            }
        }
    }

    let detail = last_error.unwrap_or_else(|| "unknown send failure".to_string());
    let journal_detail = Some(format!("failed after {ATTEMPTS} attempts: {detail}"));
    if let Ok(mut journal) = event_journal.lock()
        && let Err(error) = journal.record_outbound(group_id, text, false, journal_detail)
    {
        eprintln!("agentnoise: failed to record outbound event: {error:#}");
    }
    bail!("failed to send reply after {ATTEMPTS} attempts: {detail}")
}

fn listener_subscribe_limit(config: &Config) -> u32 {
    if config.whitenoise.require_pairing_pin && config.whitenoise.allowed_senders.is_empty() {
        config
            .whitenoise
            .subscribe_limit
            .max(FIRST_PAIRING_SUBSCRIBE_LIMIT)
    } else {
        config.whitenoise.subscribe_limit
    }
}

fn create_parallel_session(
    config_path: &Path,
    app: Arc<AgentApp>,
    wn: Arc<WnClient>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<StreamItem>,
    request: &NewSessionRequest,
    subscribe_limit: u32,
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
    subscribe_group_if_needed(wn, subscribed, tx, &group_id, subscribe_limit)?;

    Ok(group_id)
}

fn resume_parallel_session(
    config_path: &Path,
    wn: Arc<WnClient>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<StreamItem>,
    request: &agentnoise::app::ResumeSessionRequest,
    subscribe_limit: u32,
) -> Result<()> {
    persist_control_group_id(config_path, &request.group_id)?;
    subscribe_group_if_needed(wn, subscribed, tx, &request.group_id, subscribe_limit)
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
    subscribe_limit: u32,
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
        .subscribe_group_with_limit(&group_id, subscribe_limit)
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
            thread::sleep(Duration::from_secs(30));
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
