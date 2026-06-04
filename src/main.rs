use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use agentnoise::app::{AgentApp, RouteAction};
use agentnoise::attachments;
use agentnoise::auth::PairingGate;
use agentnoise::config::{Config, RunnerLauncher};
use agentnoise::darkmatter_app::DarkmatterEngine;
use agentnoise::desktop_alert;
use agentnoise::dm::DmClient;
use agentnoise::doctor::render_doctor;
use agentnoise::events::EventJournal;
use agentnoise::identity;
use agentnoise::launchd;
use agentnoise::local_sessions;
use agentnoise::queue::{JobQueue, QueuedJob};
use agentnoise::runner::{AgentKind, AgentRequest};
use agentnoise::runtime::{
    self, AcquireMode, EngineGuard, RuntimePairingInfo, RuntimePairingPin, RuntimeRole,
};
use agentnoise::service::{self, ServiceTarget};
use agentnoise::setup::{self, SetupOptions, SetupResult};
use agentnoise::text::short_ref;
use agentnoise::workspace;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerMode {
    Try,
    Wait,
    AttachIfBusy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerExecution {
    Inline,
    Queue,
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
#[command(about = "Chat with local coding agents through Marmot / Darkmatter")]
#[command(version)]
struct Cli {
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        help = "Use a named isolated instance with separate config, data, logs, keychain, and service"
    )]
    instance: Option<String>,

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
    #[command(about = "List recent local Codex/Claude sessions by metadata")]
    LocalSessions(LocalSessionsArgs),
    #[command(about = "Run an in-process Marmot v2 fake phone for local testing")]
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
    #[command(about = "Acquire the engine lock and start the Marmot v2 listener")]
    Start(StartArgs),
    #[command(about = "Run only the Marmot transport/queue listener")]
    Transport(TransportArgs),
    #[command(about = "Run local agent jobs claimed from the transport queue")]
    Worker(WorkerArgs),
    Listen,
    Send {
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    #[command(about = "Manage native launchd/systemd/rc supervisor files")]
    Service(ServiceArgs),
    Launchd(LaunchdArgs),
    #[command(about = "Marmot v2 embedded engine (darkmatter): probe + diagnostics")]
    Darkmatter(DarkmatterArgs),
    #[command(about = "Create and pair agentnoise identities")]
    Identity(IdentityArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(long)]
    force: bool,
    #[arg(
        long,
        help = "Opt into launching raw Codex/Claude/Hermes CLIs directly instead of through bondage"
    )]
    direct_agents: bool,
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(
        long,
        help = "Phone Marmot v2 npub; legacy hint (group creation is phone-initiated under v2)"
    )]
    phone: Option<String>,
    #[arg(long, help = "Unique Marmot/Nostr profile name for this machine")]
    name: Option<String>,
    #[arg(long, default_value = setup::DEFAULT_GROUP_NAME)]
    group_name: String,
    #[arg(long)]
    force_identity: bool,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(
        long,
        help = "Opt into launching raw Codex/Claude/Hermes CLIs directly instead of through bondage"
    )]
    direct_agents: bool,
    #[arg(
        long,
        help = "Development only: use Dark Matter's file-backed burner identity instead of the OS keychain"
    )]
    dev_burner_nsec: bool,
}

#[derive(Debug, Args)]
struct UpArgs {
    #[arg(
        long,
        help = "Phone Marmot v2 npub; legacy hint (group creation is phone-initiated under v2)"
    )]
    phone: Option<String>,
    #[arg(long, help = "Unique Marmot/Nostr profile name for this machine")]
    name: Option<String>,
    #[arg(long, default_value = setup::DEFAULT_GROUP_NAME)]
    group_name: String,
    #[arg(long, help = "Add a Marmot group id before starting")]
    group: Option<String>,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(long, help = "Stop after setup/group discovery instead of listening")]
    no_listen: bool,
    #[arg(long, help = "(legacy v1 flag, no-op under Marmot v2 embedded engine)")]
    no_daemon: bool,
    #[arg(
        long,
        help = "Opt into launching raw Codex/Claude/Hermes CLIs directly instead of through bondage"
    )]
    direct_agents: bool,
    #[arg(
        long,
        help = "Development only: use Dark Matter's file-backed burner identity instead of the OS keychain"
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
struct LocalSessionsArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 10)]
    limit: usize,
}

#[derive(Debug, Args)]
struct FakePhoneArgs {
    #[command(subcommand)]
    command: FakePhoneCommand,
}

#[derive(Debug, Args)]
struct TransportArgs {
    #[command(subcommand)]
    command: TransportCommand,
}

#[derive(Debug, Subcommand)]
enum TransportCommand {
    #[command(about = "Listen to Marmot and enqueue agent jobs")]
    Run(TransportRunArgs),
    #[command(about = "Show transport role status")]
    Status,
}

#[derive(Debug, Args)]
struct TransportRunArgs {
    #[arg(long, help = "Add a Marmot group id before starting")]
    group: Option<String>,
    #[arg(long, help = "Legacy v1 flag; no-op under the embedded Marmot engine")]
    no_daemon: bool,
    #[arg(
        long,
        help = "SSH setup mode: show PIN only in this terminal, not a desktop alert"
    )]
    ssh: bool,
}

#[derive(Debug, Args)]
struct WorkerArgs {
    #[command(subcommand)]
    command: WorkerCommand,
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    #[command(about = "Claim queued jobs and run local coding agents")]
    Start(WorkerStartArgs),
    #[command(about = "Show worker role status and queue counts")]
    Status,
}

#[derive(Debug, Args)]
struct WorkerStartArgs {
    #[arg(long, help = "Run exactly one queued job if present, then exit")]
    once: bool,
    #[arg(long, default_value_t = 2, help = "Idle poll interval in seconds")]
    poll_seconds: u64,
    #[arg(long, help = "Start an idempotent tmux worker session and exit")]
    tmux: bool,
}

#[derive(Debug, Args)]
struct StartArgs {
    #[arg(long, help = "Add a Marmot group id before starting")]
    group: Option<String>,
    #[arg(long, help = "(legacy v1 flag, no-op under Marmot v2 embedded engine)")]
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
    #[command(about = "Set the local agent launcher: bondage or direct")]
    Launcher {
        launcher: RunnerLauncher,
    },
    #[command(about = "Enable or disable opt-in local agent session notifications")]
    LocalSessionsWatch {
        enabled: ConfigToggle,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum ConfigToggle {
    On,
    Off,
}

impl ConfigToggle {
    fn enabled(self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Debug, Args)]
struct LaunchdArgs {
    #[command(subcommand)]
    command: LaunchdCommand,
}

#[derive(Debug, Args)]
struct DarkmatterArgs {
    #[command(subcommand)]
    command: DarkmatterCommand,
}

#[derive(Debug, Subcommand)]
enum DarkmatterCommand {
    #[command(
        about = "Bootstrap the embedded Marmot v2 engine (darkmatter) and report account state"
    )]
    Probe {
        #[arg(
            long,
            default_value = "agentnoise-desktop",
            help = "Managed account label inside the darkmatter home"
        )]
        label: String,
        #[arg(
            long = "relay",
            help = "Relay URL(s) to bootstrap with; defaults to config message_relays when empty"
        )]
        relays: Vec<String>,
        #[arg(
            long,
            help = "Path to the darkmatter home; defaults to <data-dir>/darkmatter"
        )]
        home: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct IdentityArgs {
    #[command(subcommand)]
    command: IdentityCommand,
}

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
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
        #[arg(
            long,
            help = "Text that must appear in at least one observed reply; repeatable"
        )]
        expect: Vec<String>,
        #[arg(long, default_value_t = 1)]
        min_replies: usize,
        #[arg(long, help = "Require a final job reply, not only the initial ack")]
        require_job_final: bool,
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
    #[command(about = "Change the configured Marmot/Nostr profile name for this machine")]
    Rename {
        name: String,
        #[arg(
            long,
            help = "Save config only; the next listener startup re-publishes"
        )]
        no_publish: bool,
    },
    #[command(about = "Show the configured desktop identity npub")]
    Show,
    #[command(about = "Render a phone-pairing QR for the configured desktop identity")]
    Qr {
        #[arg(long = "relay")]
        relays: Vec<String>,
    },
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
    init_logging();
    let cli = Cli::parse_from(normalized_cli_args());
    if cli.config.is_some() && cli.instance.is_some() {
        bail!("--instance cannot be combined with --config");
    }
    let instance = cli
        .instance
        .as_deref()
        .map(|name| {
            agentnoise::paths::normalize_instance_name(name)
                .with_context(|| format!("invalid instance name: {name}"))
        })
        .transpose()?;
    let config_path = Config::path_or_default(cli.config, instance.as_deref());

    match cli.command {
        Command::Init(args) => {
            Config::write_template(
                &config_path,
                args.force,
                if args.direct_agents {
                    RunnerLauncher::Direct
                } else {
                    RunnerLauncher::Bondage
                },
            )?;
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
                    direct_agents: args.direct_agents,
                    dev_burner_nsec: args.dev_burner_nsec,
                },
            )?;
            print_setup_result(&result);
        }
        Command::Up(args) => {
            up(&config_path, args)?;
        }
        Command::Pair(args) => {
            pair_command(&config_path, args)?;
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
        Command::LocalSessions(args) => {
            let sessions =
                agentnoise::local_sessions::discover_local_sessions(args.limit.clamp(1, 100))?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else {
                println!("{}", agentnoise::local_sessions::render_sessions(&sessions));
            }
        }
        Command::FakePhone(args) => {
            fake_phone_command(&config_path, args)?;
        }
        Command::Config(args) => match args.command {
            ConfigCommand::Path => println!("{}", config_path.display()),
            ConfigCommand::PrintTemplate => println!(
                "{}",
                toml::to_string_pretty(&Config::template_for_path(&config_path))
                    .context("serializing template config")?
            ),
            ConfigCommand::Launcher { launcher } => {
                let mut config = Config::load_or_template(&config_path)?;
                config.runner.launcher = launcher;
                config.save(&config_path)?;
                println!("agent launcher: {}", config.runner.launcher);
            }
            ConfigCommand::LocalSessionsWatch { enabled } => {
                let mut config = Config::load_or_template(&config_path)?;
                config.local_sessions.watch = enabled.enabled();
                config.save(&config_path)?;
                println!(
                    "local session watch: {}",
                    if config.local_sessions.watch {
                        "on"
                    } else {
                        "off"
                    }
                );
            }
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
                    println!("{}", request.created_text());
                    println!("New chat: {}", request.group_name());
                    println!("{}", request.ready_text());
                    println!(
                        "Note: `agentnoise handle` does not create the Marmot chat; run this from the live listener for real delivery."
                    );
                }
                RouteAction::ResumeSession(request) => {
                    println!("{}", request.reply_text);
                    println!("Target chat: {}", request.group_id);
                    println!("{}", request.target_text);
                }
                RouteAction::IngestAttachments(request) => {
                    println!(
                        "Attachment ingest requested for {}. Run this from the live listener for real delivery.",
                        attachments::render_record_summary(&request.record)
                    );
                }
                RouteAction::UploadMedia(request) => {
                    println!(
                        "Upload requested for {}. Run this from the live listener for real delivery.",
                        request.path.display()
                    );
                }
                RouteAction::DownloadMedia(request) => {
                    println!(
                        "Download requested for {}. Run this from the live listener for real delivery.",
                        request.output_path.display()
                    );
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
            start_listener(
                &config_path,
                args,
                ListenerMode::Try,
                ListenerExecution::Inline,
            )?;
        }
        Command::Transport(args) => {
            transport_command(&config_path, args)?;
        }
        Command::Worker(args) => {
            worker_command(&config_path, args)?;
        }
        Command::Listen => {
            let config = Config::load(&config_path)?;
            let pairing = pairing_for_listener(&config_path, &config, PairingDisplay::Desktop)?;
            run_listener_with_mode(
                &config_path,
                config,
                pairing,
                ListenerMode::Try,
                ListenerExecution::Inline,
            )?;
        }
        Command::Send { text: _ } => {
            bail!(
                "`agentnoise send` is being rewritten for darkmatter — the v2 listener owns the \
                 send path. For now use `agentnoise darkmatter probe` to validate the engine and \
                 check docs/darkmatter.md for status."
            );
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
                let config = Config::load_or_template(&config_path)?;
                let removed = launchd::uninstall(&config, unload)?;
                if removed {
                    println!("removed {}", launchd::plist_path(&config).display());
                } else {
                    println!("not installed");
                }
            }
        },
        Command::Darkmatter(args) => {
            darkmatter_command(&config_path, args)?;
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
                IdentityCommand::Show => match config.darkmatter.account.as_deref() {
                    Some(npub) => println!("npub: {npub}"),
                    None => println!(
                        "no desktop identity configured yet; run `agentnoise setup` or `agentnoise listen` once"
                    ),
                },
                IdentityCommand::Qr { relays } => {
                    let Some(npub) = config.darkmatter.account.as_deref() else {
                        bail!(
                            "no desktop identity configured yet; run `agentnoise setup` or `agentnoise listen` once"
                        );
                    };
                    let payload = identity::pairing_payload_from_npub(
                        &config.darkmatter,
                        identity::DEFAULT_IDENTITY_NAME,
                        npub,
                        &relays,
                    )?;
                    println!("agentnoise pairing");
                    println!("name: {}", payload.name);
                    println!("npub: {}", payload.npub);
                    println!("nprofile: {}", payload.nprofile);
                    println!("relays:");
                    for relay in &payload.relays {
                        println!("- {relay}");
                    }
                    println!();
                    println!("{}", identity::render_qr(&payload.npub)?);
                }
            }
        }
    }

    Ok(())
}

fn init_logging() {
    let filter = std::env::var("AGENTNOISE_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "warn".to_string());
    let Ok(filter) = tracing_subscriber::EnvFilter::try_new(filter) else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
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
    if result.dev_burner_nsec {
        println!("secret store: file-backed dev burner identity");
        println!(
            "warning: development-only plaintext secret store; do not use for a real identity"
        );
    }
    if result.created_config {
        println!("config file: created");
    }
    println!("relays:");
    for relay in &result.relays {
        println!("- {relay}");
    }
    println!();
    println!("{}", result.qr);
    println!(
        "next: have the phone scan this QR and start a Marmot group with the desktop identity."
    );
    println!(
        "Then run: agentnoise listen (or `agentnoise darkmatter probe` to smoke-test the engine first)."
    );
}

fn print_identity_status(config: &Config) {
    println!("identity: {}", identity::DEFAULT_IDENTITY_NAME);
    println!("profile name: {}", config.darkmatter.profile_name);
    println!(
        "profile display: {}",
        config.darkmatter.profile_display_name
    );
    println!("profile about: {}", config.darkmatter.profile_about);
    if config.darkmatter.dev_burner_nsec {
        println!("secret store: file-backed dev burner identity");
    } else {
        println!("secret store: OS keychain (service: \"agentnoise\", item: <account_id_hex>)");
    }
    if let Some(npub) = config
        .darkmatter
        .account
        .as_deref()
        .or(config.darkmatter.bot_npub.as_deref())
    {
        println!("npub: {npub}");
    } else {
        println!("npub: unavailable; run `agentnoise listen` once to create the desktop identity");
    }
    let groups = config.darkmatter.control_group_ids();
    println!("groups: {}", groups.len());
    println!(
        "allowed senders: {}",
        config.darkmatter.allowed_senders.len()
    );
    println!("pairing relays:");
    for relay in identity::pairing_relays(&config.darkmatter, &[]) {
        println!("- {relay}");
    }
    println!("message relays:");
    for relay in &config.darkmatter.message_relays {
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

    config.darkmatter.profile_name = setup::normalize_profile_name(display_name);
    config.darkmatter.profile_display_name = display_name.to_string();
    config.save(config_path)?;

    println!("profile name: {}", config.darkmatter.profile_name);
    println!(
        "profile display: {}",
        config.darkmatter.profile_display_name
    );
    if no_publish {
        println!("profile: saved; next `agentnoise listen` startup publishes it");
        return Ok(());
    }

    // The embedded engine publishes the user profile (Nostr kind 0) on
    // listener startup via `runtime.publish_user_profile`. We don't have
    // a long-lived engine here, so we just note that the publish happens
    // on next listen.
    println!("profile: saved; restart `agentnoise listen` to publish the new label to relays");
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
            let config = Config::load_or_template(config_path)?;
            match service::uninstall(target, &config, unload, path.as_deref())? {
                Some(path) => println!("removed {}", path.display()),
                None => println!("not installed"),
            }
        }
    }
    Ok(())
}

fn pair_command(config_path: &Path, args: PairArgs) -> Result<()> {
    let config = Config::load_or_template(config_path)?;
    if runtime::engine_is_running(&config)?
        && let Some(status) = runtime::read_status(&config)?
        && let Some(pairing) = status.pairing
    {
        print_pairing_details(
            &pairing.npub,
            &pairing.nprofile,
            pairing.current_pin.as_ref(),
            Some("pairing PIN: waiting for listener update"),
        )?;
        return Ok(());
    }

    let payload = setup::pairing(config_path, &args.relays)?;
    print_pairing_details(
        &payload.npub,
        &payload.nprofile,
        None,
        Some("pairing PIN: unavailable until `agentnoise up` is running"),
    )?;
    Ok(())
}

fn print_pairing_details(
    npub: &str,
    nprofile: &str,
    pin: Option<&RuntimePairingPin>,
    missing_pin_text: Option<&str>,
) -> Result<()> {
    println!("agentnoise pairing");
    println!("npub: {npub}");
    println!("nprofile: {nprofile}");
    match pin {
        Some(pin) => runtime::print_runtime_pairing_pin(pin),
        None => {
            if let Some(text) = missing_pin_text {
                println!("{text}");
            }
        }
    }
    println!();
    println!("{}", identity::render_qr(npub)?);
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
            direct_agents: args.direct_agents,
            dev_burner_nsec: args.dev_burner_nsec,
        },
    )?;

    let mut config = Config::load(config_path)?;
    if let Some(group) = args
        .group
        .as_deref()
        .map(str::trim)
        .filter(|group| !group.is_empty())
    {
        config.darkmatter.add_control_group_id(group);
        config.save(config_path)?;
    }

    if !config.darkmatter.has_control_group() {
        match discover_group(&mut config, config_path) {
            Err(error) => {
                print_setup_result(&result);
                eprintln!("agentnoise: group discovery failed: {error:#}");
                return Ok(());
            }
            Ok(GroupDiscovery::Ready) => {
                println!("agentnoise: discovered Marmot control chat(s)");
            }
            Ok(GroupDiscovery::NeedsPairing) => {
                print_setup_result(&result);
                println!();
                if args.no_listen {
                    println!("next: scan the QR, create a Marmot chat with agentnoise, then run:");
                    println!("agentnoise up");
                    return Ok(());
                }
                println!("agentnoise: waiting for a Marmot control chat");
                println!("agentnoise: scan the QR, create the chat, then send the pairing PIN");
            }
        }
    }

    let groups = config.darkmatter.control_group_ids();
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
    if config.darkmatter.allowed_senders.is_empty() && config.darkmatter.require_pairing_pin {
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
            // setup() already performed daemon startup/login/profile repair for `up`.
            no_daemon: true,
            ssh: args.ssh,
        },
        if args.ssh {
            ListenerMode::Try
        } else {
            up_listener_mode()
        },
        ListenerExecution::Inline,
    )
}

fn should_attach_before_setup(config_path: &Path, args: &UpArgs) -> Result<bool> {
    if !runtime::stdio_is_interactive()
        || args.no_listen
        || args.phone.is_some()
        || args.group.is_some()
        || args.name.is_some()
        || !args.relays.is_empty()
        || args.direct_agents
        || args.dev_burner_nsec
        || args.ssh
        || !config_path.exists()
    {
        return Ok(false);
    }

    let config = Config::load(config_path)?;
    Ok(runtime::engine_is_running(&config)?
        || runtime::role_is_running(&config, RuntimeRole::Transport)?)
}

fn darkmatter_command(config_path: &Path, args: DarkmatterArgs) -> Result<()> {
    match args.command {
        DarkmatterCommand::Probe {
            label,
            relays,
            home,
            json,
        } => {
            let config = Config::load_or_template(config_path)?;
            let resolved_home =
                home.unwrap_or_else(|| config.resolved_data_dir().join("darkmatter"));
            let bootstrap_relays: Vec<String> = if relays.is_empty() {
                config.darkmatter.message_relays.clone()
            } else {
                relays
            };
            if bootstrap_relays.is_empty() {
                bail!(
                    "no relay urls available: pass --relay <url> or configure darkmatter.message_relays"
                );
            }

            let keychain_service = agentnoise::darkmatter_app::keychain_service_for_instance(
                config.instance.as_deref(),
            );

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("building tokio runtime for darkmatter probe")?;

            // Reuse the configured desktop account when present so probe
            // reports the same identity the listener uses, instead of minting
            // a throwaway one.
            let configured_account = config.darkmatter.account.clone();

            rt.block_on(async {
                let engine = DarkmatterEngine::open(
                    resolved_home,
                    bootstrap_relays.clone(),
                    &keychain_service,
                    config.darkmatter.dev_burner_nsec,
                )?;
                engine.start().await?;
                let account_id_hex = engine
                    .ensure_account(configured_account.as_deref(), &bootstrap_relays)
                    .await?;

                let report = DarkmatterProbeReport {
                    home: engine.home().display().to_string(),
                    account_label: label,
                    account_id_hex,
                    relays: bootstrap_relays,
                };

                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("darkmatter engine started");
                    println!("home:    {}", report.home);
                    println!("label:   {}", report.account_label);
                    println!("account: {}", report.account_id_hex);
                    println!("relays:  {}", report.relays.join(", "));
                }

                engine.shutdown().await;
                anyhow::Ok(())
            })
        }
    }
}

#[derive(serde::Serialize)]
struct DarkmatterProbeReport {
    home: String,
    account_label: String,
    account_id_hex: String,
    relays: Vec<String>,
}

fn fake_phone_command(config_path: &Path, args: FakePhoneArgs) -> Result<()> {
    let config = Config::load_or_template(config_path)?;
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
            expect,
            min_replies,
            require_job_final,
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
                    expect,
                    min_replies,
                    require_job_final,
                },
            )?;
            println!("fake phone npub: {}", result.phone_npub);
            println!("group: {}", result.group_id);
            println!(
                "job final: {}",
                if result.saw_job_final { "yes" } else { "no" }
            );
            if !result.matched.is_empty() {
                println!("matched:");
                for matched in result.matched {
                    println!("- {matched}");
                }
            }
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

fn transport_command(config_path: &Path, args: TransportArgs) -> Result<()> {
    match args.command {
        TransportCommand::Run(args) => transport_run(config_path, args),
        TransportCommand::Status => {
            let config = Config::load_or_template(config_path)?;
            print_role_status(&config, RuntimeRole::Transport)?;
            Ok(())
        }
    }
}

fn transport_run(config_path: &Path, args: TransportRunArgs) -> Result<()> {
    let mut config = Config::load(config_path)?;
    if let Some(group) = args
        .group
        .as_deref()
        .map(str::trim)
        .filter(|group| !group.is_empty())
    {
        config.darkmatter.add_control_group_id(group);
        config.save(config_path)?;
    }

    let mode = if runtime::stdio_is_interactive() {
        ListenerMode::AttachIfBusy
    } else {
        ListenerMode::Wait
    };
    let guard = acquire_role_guard(config_path, &config, RuntimeRole::Transport, mode)?;
    if guard.is_none() {
        print_role_status(&config, RuntimeRole::Transport)?;
        return Ok(());
    }
    let _role_guard = guard;
    let engine_guard = runtime::acquire_engine(config_path, &config, AcquireMode::Try)?
        .ok_or_else(|| anyhow::anyhow!("inline listener started while transport was starting"))?;

    if !args.no_daemon {
        eprintln!("agentnoise: darkmatter engine starts inline with the transport");
    }

    let pairing_display = if args.ssh {
        PairingDisplay::TerminalOnly
    } else {
        PairingDisplay::Desktop
    };
    let pairing = pairing_for_listener(config_path, &config, pairing_display)?;
    if let Some(pairing) = pairing_runtime_info(&config, pairing.as_ref()) {
        engine_guard.update_status(config_path, &config, Some(pairing))?;
    }
    run_listener(
        config_path,
        config,
        pairing,
        engine_guard,
        ListenerExecution::Queue,
    )
}

fn worker_command(config_path: &Path, args: WorkerArgs) -> Result<()> {
    match args.command {
        WorkerCommand::Start(args) => worker_start(config_path, args),
        WorkerCommand::Status => {
            let config = Config::load_or_template(config_path)?;
            print_role_status(&config, RuntimeRole::Worker)?;
            print_queue_status(&config)?;
            Ok(())
        }
    }
}

fn worker_start(config_path: &Path, args: WorkerStartArgs) -> Result<()> {
    let config = Config::load(config_path)?;
    if args.tmux {
        return start_worker_tmux(config_path, &config);
    }

    let guard = runtime::acquire_role(config_path, &config, RuntimeRole::Worker, AcquireMode::Try)?;
    let Some(_guard) = guard else {
        match runtime::role_lock_owner(&config, RuntimeRole::Worker)? {
            Some(pid) => println!("agentnoise worker already running as pid {pid}"),
            None => println!("agentnoise worker already running"),
        }
        return Ok(());
    };

    let dm_runtime = open_dm_runtime(&config, "worker")?;
    let queue = JobQueue::open(config.resolved_queue_path())?;
    let app = Arc::new(AgentApp::from_config_path(config_path)?);
    let event_journal = Arc::new(Mutex::new(EventJournal::open(
        &app.config().resolved_event_log_path(),
    )?));
    let runner = DmJobRunner {
        app: Arc::clone(&app),
        dm: Arc::clone(&dm_runtime.dm),
        engine: dm_runtime.engine.clone(),
        account_id_hex: dm_runtime.account_id_hex.clone(),
        handle: dm_runtime.handle.clone(),
        event_journal: Arc::clone(&event_journal),
    };
    let worker_id = format!("worker:{}", std::process::id());
    let idle_delay = Duration::from_secs(args.poll_seconds.max(1));

    println!("agentnoise worker running");
    loop {
        match queue.claim_next(&worker_id)? {
            Some(job) => run_queued_dm_job(&queue, runner.clone(), job)?,
            None if args.once => {
                println!("agentnoise worker: no queued jobs");
                dm_runtime.shutdown();
                return Ok(());
            }
            None => thread::sleep(idle_delay),
        }
        if args.once {
            dm_runtime.shutdown();
            return Ok(());
        }
    }
}

fn start_worker_tmux(config_path: &Path, config: &Config) -> Result<()> {
    if runtime::role_is_running(config, RuntimeRole::Worker)? {
        match runtime::role_lock_owner(config, RuntimeRole::Worker)? {
            Some(pid) => println!("agentnoise worker already running as pid {pid}"),
            None => println!("agentnoise worker already running"),
        }
        return Ok(());
    }
    ensure_tmux_available()?;

    let exe = std::env::current_exe().context("resolving current executable")?;
    let session = config
        .instance
        .as_deref()
        .map(|instance| format!("agentnoise-worker-{instance}"))
        .unwrap_or_else(|| "agentnoise-worker".to_string());
    let status = ProcessCommand::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&session)
        .arg(exe)
        .arg("--config")
        .arg(config_path)
        .arg("worker")
        .arg("start")
        .status()
        .context("starting tmux worker session")?;
    if !status.success() {
        bail!("tmux new-session exited with {status}");
    }
    println!("agentnoise worker tmux session: {session}");
    Ok(())
}

fn ensure_tmux_available() -> Result<()> {
    match ProcessCommand::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("tmux -V exited with {status}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("tmux not found; install tmux or run `agentnoise worker start` in a terminal")
        }
        Err(error) => Err(error).context("checking tmux"),
    }
}

fn acquire_role_guard(
    config_path: &Path,
    config: &Config,
    role: RuntimeRole,
    mode: ListenerMode,
) -> Result<Option<agentnoise::runtime::RoleGuard>> {
    match mode {
        ListenerMode::Try => runtime::acquire_role(config_path, config, role, AcquireMode::Try)?
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("agentnoise {role:?} is already running")),
        ListenerMode::Wait => runtime::acquire_role(config_path, config, role, AcquireMode::Wait),
        ListenerMode::AttachIfBusy => {
            runtime::acquire_role(config_path, config, role, AcquireMode::Try)
        }
    }
}

fn print_role_status(config: &Config, role: RuntimeRole) -> Result<()> {
    let running = runtime::role_is_running(config, role)?;
    println!(
        "{}: {}",
        role.as_str(),
        if running { "running" } else { "stopped" }
    );
    if let Some(status) = runtime::read_role_status(config, role)? {
        println!("pid: {}", status.pid);
        println!("started: {}", status.started_at);
    }
    Ok(())
}

fn print_queue_status(config: &Config) -> Result<()> {
    let queue = JobQueue::open(config.resolved_queue_path())?;
    let counts = queue.counts()?;
    println!("queue: {}", queue.path().display());
    println!("queued: {}", counts.queued);
    println!("claimed: {}", counts.claimed);
    println!("running: {}", counts.running);
    println!("succeeded: {}", counts.succeeded);
    println!("failed: {}", counts.failed);
    Ok(())
}

fn wait_before_setup_if_needed(config_path: &Path, args: &UpArgs) -> Result<()> {
    if runtime::stdio_is_interactive()
        || args.no_listen
        || args.phone.is_some()
        || args.group.is_some()
        || args.name.is_some()
        || !args.relays.is_empty()
        || args.ssh
        || !config_path.exists()
    {
        return Ok(());
    }

    let config = Config::load(config_path)?;
    let mut last_notice = Instant::now() - Duration::from_secs(30);
    while runtime::engine_is_running(&config)? {
        if last_notice.elapsed() >= Duration::from_secs(30) {
            match runtime::engine_lock_owner(&config)? {
                Some(pid) => eprintln!(
                    "agentnoise: another listener is running as pid {pid}; service startup is waiting for it to exit"
                ),
                None => eprintln!(
                    "agentnoise: another listener is running; service startup is waiting for it to exit"
                ),
            }
            last_notice = Instant::now();
        }
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

fn discover_group(config: &mut Config, _config_path: &Path) -> Result<GroupDiscovery> {
    // Under v2, group discovery is event-driven via `MarmotAppEvent::GroupJoined`
    // from the embedded `MarmotAppRuntime`. Until the listener fully ports,
    // we rely on the configured control group id (set after first pairing).
    if config.darkmatter.has_control_group() {
        Ok(GroupDiscovery::Ready)
    } else {
        Ok(GroupDiscovery::NeedsPairing)
    }
}

fn start_listener(
    config_path: &Path,
    args: StartArgs,
    mode: ListenerMode,
    execution: ListenerExecution,
) -> Result<()> {
    let mut config = Config::load(config_path)?;
    if let Some(group) = args
        .group
        .as_deref()
        .map(str::trim)
        .filter(|group| !group.is_empty())
    {
        config.darkmatter.add_control_group_id(group);
        config.save(config_path)?;
    }

    let guard = acquire_listener_guard(config_path, &config, mode)?;
    let Some(guard) = guard else {
        runtime::attach_ui(config_path, &config)?;
        return Ok(());
    };

    if !args.no_daemon {
        // v1 used to spawn `wnd` here. The v2 embedded engine starts inside
        // run_listener via `DarkmatterEngine::start`.
        eprintln!("agentnoise: darkmatter engine starts inline with the listener");
    }

    let pairing_display = if args.ssh {
        PairingDisplay::TerminalOnly
    } else {
        PairingDisplay::Desktop
    };
    let pairing = pairing_for_listener(config_path, &config, pairing_display)?;
    if let Some(pairing) = pairing_runtime_info(&config, pairing.as_ref()) {
        guard.update_status(config_path, &config, Some(pairing))?;
    }
    run_listener(config_path, config, pairing, guard, execution)
}

fn pairing_for_listener(
    config_path: &Path,
    config: &Config,
    display: PairingDisplay,
) -> Result<Option<PairingRuntime>> {
    if !config.darkmatter.require_pairing_pin || !config.darkmatter.allowed_senders.is_empty() {
        return Ok(None);
    }

    let gate = PairingGate::new(config.darkmatter.pairing_pin_seconds);
    let payload = setup::pairing(config_path, &[])?;
    println!("agentnoise pairing required");
    println!("QR contains the desktop npub. The printed nprofile includes relay hints.");
    println!("npub: {}", payload.npub);
    println!("nprofile: {}", payload.nprofile);
    println!();
    println!("{}", identity::render_qr(&payload.npub)?);
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
    execution: ListenerExecution,
) -> Result<()> {
    let guard = acquire_listener_guard(config_path, &config, mode)?;
    let Some(guard) = guard else {
        runtime::attach_ui(config_path, &config)?;
        return Ok(());
    };
    if let Some(pairing) = pairing_runtime_info(&config, pairing.as_ref()) {
        guard.update_status(config_path, &config, Some(pairing))?;
    }
    run_listener(config_path, config, pairing, guard, execution)
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
        pin_seconds: config.darkmatter.pairing_pin_seconds,
        current_pin: None,
    })
}

fn run_listener(
    config_path: &Path,
    config: Config,
    pairing: Option<PairingRuntime>,
    _guard: EngineGuard,
    execution: ListenerExecution,
) -> Result<()> {
    let dm_home = config.resolved_data_dir().join("darkmatter");
    let bootstrap_relays = config.darkmatter.message_relays.clone();
    if bootstrap_relays.is_empty() {
        bail!(
            "darkmatter.message_relays is empty; set at least one relay in {} before listening",
            config_path.display()
        );
    }

    let keychain_service =
        agentnoise::darkmatter_app::keychain_service_for_instance(config.instance.as_deref());

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for the darkmatter listener")?;
    let tokio_handle = tokio_runtime.handle().clone();

    let engine = DarkmatterEngine::open(
        dm_home,
        bootstrap_relays.clone(),
        &keychain_service,
        config.darkmatter.dev_burner_nsec,
    )?;
    tokio_handle.block_on(engine.start())?;
    let account_id_hex = tokio_handle
        .block_on(engine.ensure_account(config.darkmatter.account.as_deref(), &bootstrap_relays))?;
    eprintln!("agentnoise: darkmatter account ready: {account_id_hex}");
    spawn_darkmatter_discovery_broadcast(
        &tokio_handle,
        engine.clone(),
        account_id_hex.clone(),
        bootstrap_relays.clone(),
    );

    if let Some(pairing_runtime) = pairing.clone() {
        spawn_pairing_pin_display(config.clone(), pairing_runtime);
    }

    let app = Arc::new(AgentApp::new_with_auth(
        config_path.to_path_buf(),
        config.clone(),
        pairing.map(|pairing| pairing.gate),
    )?);
    let recovered = app.recover_interrupted_jobs()?;
    if recovered > 0 {
        eprintln!("agentnoise: marked {recovered} unfinished job(s) interrupted after restart");
    }

    let event_journal = Arc::new(Mutex::new(EventJournal::open(
        &app.config().resolved_event_log_path(),
    )?));
    let job_queue = if execution == ListenerExecution::Queue {
        Some(JobQueue::open(app.config().resolved_queue_path())?)
    } else {
        None
    };

    let max_chars = config.darkmatter.max_message_chars;
    let dm = Arc::new(DmClient::new(
        engine.clone(),
        account_id_hex.clone(),
        max_chars,
        tokio_handle.clone(),
    ));
    let job_runner = DmJobRunner {
        app: Arc::clone(&app),
        dm: Arc::clone(&dm),
        engine: engine.clone(),
        account_id_hex: account_id_hex.clone(),
        handle: tokio_handle.clone(),
        event_journal: Arc::clone(&event_journal),
    };

    let group_ids = config.darkmatter.control_group_ids();
    if group_ids.is_empty() {
        println!(
            "agentnoise listening (no paired group yet — show the QR and let the phone create one)"
        );
    } else {
        for group_id in &group_ids {
            println!("agentnoise listening on group {group_id}");
        }
    }

    let (tx, rx) = mpsc::channel::<agentnoise::dm::MessageEvent>();
    for group_id in &group_ids {
        let dm_clone = Arc::clone(&dm);
        let tx_clone = tx.clone();
        let group_id = group_id.clone();
        tokio_handle.spawn(async move {
            let mut subscription = match dm_clone.subscribe_group(&group_id).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    eprintln!("agentnoise: failed to subscribe to {group_id}: {error:#}");
                    return;
                }
            };
            for event in subscription.snapshot() {
                if tx_clone.send(event).is_err() {
                    return;
                }
            }
            while let Some(event) = subscription.next_message().await {
                if tx_clone.send(event).is_err() {
                    return;
                }
            }
        });
    }
    spawn_local_session_watcher_simple(config_path, &config, &dm, &event_journal);
    spawn_group_join_discovery(
        &tokio_handle,
        engine.clone(),
        Arc::clone(&dm),
        account_id_hex.clone(),
        config_path.to_path_buf(),
        group_ids.iter().cloned().collect(),
        tx.clone(),
    );
    drop(tx);

    let ignore_initial = config.darkmatter.ignore_initial_messages;

    for event in rx {
        let Some(group_id) = event.group_id.clone() else {
            continue;
        };
        {
            let mut journal = event_journal
                .lock()
                .map_err(|_| anyhow::anyhow!("event journal lock poisoned"))?;
            if journal.already_seen(&group_id, event.id.as_deref()) {
                continue;
            }
            if let Err(error) = journal.record_inbound(&event) {
                eprintln!("agentnoise: failed to record inbound event: {error:#}");
            }
        }
        if ignore_initial && event.is_initial {
            continue;
        }

        if event.attachments.is_empty()
            && let Some(message) = event.unsupported.as_deref()
        {
            let action = match app.route_unsupported_message(event.sender.as_deref(), message) {
                Ok(action) => action,
                Err(error) => {
                    eprintln!("agentnoise: routing failed: {error:#}");
                    continue;
                }
            };
            match action {
                RouteAction::Ignore => {}
                RouteAction::Reply(reply) => {
                    try_send_dm_reply_recorded(&dm, &event_journal, &group_id, &reply);
                }
                RouteAction::IngestAttachments(_)
                | RouteAction::NewSession(_)
                | RouteAction::ResumeSession(_)
                | RouteAction::UploadMedia(_)
                | RouteAction::DownloadMedia(_)
                | RouteAction::Run(_) => {}
            }
            continue;
        }

        let mut attachment_ingest = None;
        if !event.attachments.is_empty() {
            let action = match app.route_unsupported_event(&event) {
                Ok(action) => action,
                Err(error) => {
                    eprintln!("agentnoise: routing failed: {error:#}");
                    continue;
                }
            };
            match action {
                RouteAction::Ignore => continue,
                RouteAction::Reply(reply) => {
                    try_send_dm_reply_recorded(&dm, &event_journal, &group_id, &reply);
                    continue;
                }
                RouteAction::IngestAttachments(request) => {
                    let ingest = ingest_dm_attachments(
                        &app,
                        &dm,
                        &group_id,
                        event.sender.as_deref(),
                        request,
                    );
                    if event.text.trim().is_empty() {
                        try_send_dm_reply_recorded(
                            &dm,
                            &event_journal,
                            &group_id,
                            &ingest.reply_text(),
                        );
                        continue;
                    }
                    attachment_ingest = Some(ingest);
                }
                RouteAction::NewSession(_)
                | RouteAction::ResumeSession(_)
                | RouteAction::UploadMedia(_)
                | RouteAction::DownloadMedia(_)
                | RouteAction::Run(_) => {}
            }
        }

        let mut action = match app.route_message(
            event.group_id.as_deref(),
            event.sender.as_deref(),
            &event.text,
        ) {
            Ok(action) => action,
            Err(error) => {
                eprintln!("agentnoise: routing failed: {error:#}");
                continue;
            }
        };
        if let Some(ingest) = &attachment_ingest {
            action = action_with_attachment_context(action, ingest);
        }

        if let Some(queue) = job_queue.as_ref()
            && should_intercept_active_followup(
                &action,
                &event,
                &group_id,
                &app.config().darkmatter.group_id,
            )
        {
            match queue.active_for_reply_group(&group_id) {
                Ok(Some(job)) => {
                    try_send_dm_reply_recorded(
                        &dm,
                        &event_journal,
                        &group_id,
                        &active_job_followup_text(&job),
                    );
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "agentnoise: failed to check active queue job for {group_id}: {error:#}"
                    );
                }
            }
        }

        match action {
            RouteAction::Ignore => {}
            RouteAction::Reply(reply) => {
                try_send_dm_reply_recorded(&dm, &event_journal, &group_id, &reply);
            }
            RouteAction::NewSession(_) | RouteAction::ResumeSession(_) => {
                try_send_dm_reply_recorded(
                    &dm,
                    &event_journal,
                    &group_id,
                    "agentnoise: parallel/resume sessions are not yet wired through darkmatter v2 (Phase 3 follow-up)",
                );
            }
            RouteAction::IngestAttachments(_) => {}
            RouteAction::UploadMedia(request) => {
                try_upload_dm_media(&dm, &event_journal, &group_id, request);
            }
            RouteAction::DownloadMedia(request) => {
                try_download_dm_media(&app, &dm, &event_journal, &group_id, request);
            }
            RouteAction::Run(request) => {
                try_send_dm_reply_recorded(
                    &dm,
                    &event_journal,
                    &group_id,
                    &app.run_ack_text(&request),
                );
                dispatch_dm_agent_request(DmAgentDispatch {
                    execution,
                    job_queue: job_queue.as_ref(),
                    runner: job_runner.clone(),
                    event: &event,
                    source_group_id: &group_id,
                    reply_group_id: group_id.clone(),
                    request,
                });
            }
        }
    }

    tokio_handle.block_on(engine.shutdown());
    drop(tokio_runtime);
    Ok(())
}

fn spawn_darkmatter_discovery_broadcast(
    tokio_handle: &tokio::runtime::Handle,
    engine: DarkmatterEngine,
    account_id_hex: String,
    relays: Vec<String>,
) {
    tokio_handle.spawn(async move {
        match tokio::time::timeout(
            Duration::from_secs(30),
            engine.publish_discovery(&account_id_hex, &relays),
        )
        .await
        {
            Ok(Ok(())) => {
                eprintln!("agentnoise: darkmatter discovery broadcast complete");
            }
            Ok(Err(error)) => {
                eprintln!("agentnoise: darkmatter discovery broadcast failed: {error:#}");
            }
            Err(_) => {
                eprintln!("agentnoise: darkmatter discovery broadcast timed out; continuing");
            }
        }
    });
}

struct OpenDmRuntime {
    runtime: tokio::runtime::Runtime,
    handle: tokio::runtime::Handle,
    engine: DarkmatterEngine,
    account_id_hex: String,
    dm: Arc<DmClient>,
}

impl OpenDmRuntime {
    fn shutdown(self) {
        self.handle.block_on(self.engine.shutdown());
        drop(self.runtime);
    }
}

#[derive(Clone)]
struct DmJobRunner {
    app: Arc<AgentApp>,
    dm: Arc<DmClient>,
    engine: DarkmatterEngine,
    account_id_hex: String,
    handle: tokio::runtime::Handle,
    event_journal: Arc<Mutex<EventJournal>>,
}

fn open_dm_runtime(config: &Config, label: &str) -> Result<OpenDmRuntime> {
    let dm_home = config.resolved_data_dir().join("darkmatter");
    let bootstrap_relays = config.darkmatter.message_relays.clone();
    if bootstrap_relays.is_empty() {
        bail!("darkmatter.message_relays is empty; set at least one relay before starting {label}");
    }
    let keychain_service =
        agentnoise::darkmatter_app::keychain_service_for_instance(config.instance.as_deref());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .with_context(|| format!("building tokio runtime for darkmatter {label}"))?;
    let handle = runtime.handle().clone();
    let engine = DarkmatterEngine::open(
        dm_home,
        bootstrap_relays.clone(),
        &keychain_service,
        config.darkmatter.dev_burner_nsec,
    )?;
    handle.block_on(engine.start())?;
    let account_id_hex = handle
        .block_on(engine.ensure_account(config.darkmatter.account.as_deref(), &bootstrap_relays))?;
    let dm = Arc::new(DmClient::new(
        engine.clone(),
        account_id_hex.clone(),
        config.darkmatter.max_message_chars,
        handle.clone(),
    ));
    Ok(OpenDmRuntime {
        runtime,
        handle,
        engine,
        account_id_hex,
        dm,
    })
}

struct DmAgentDispatch<'a> {
    execution: ListenerExecution,
    job_queue: Option<&'a JobQueue>,
    runner: DmJobRunner,
    event: &'a agentnoise::dm::MessageEvent,
    source_group_id: &'a str,
    reply_group_id: String,
    request: AgentRequest,
}

fn dispatch_dm_agent_request(dispatch: DmAgentDispatch<'_>) {
    match dispatch.execution {
        ListenerExecution::Inline => {
            run_inline_dm_job(dispatch.runner, dispatch.reply_group_id, dispatch.request);
        }
        ListenerExecution::Queue => {
            let Some(queue) = dispatch.job_queue else {
                try_send_dm_reply_recorded(
                    &dispatch.runner.dm,
                    &dispatch.runner.event_journal,
                    &dispatch.reply_group_id,
                    "Error: transport queue is not open.",
                );
                return;
            };
            let source_event_id = queue_source_event_id(dispatch.event, dispatch.source_group_id);
            match queue.enqueue(
                &dispatch.request,
                dispatch.source_group_id,
                &dispatch.reply_group_id,
                &source_event_id,
            ) {
                Ok(outcome) => {
                    if !outcome.inserted {
                        try_send_dm_reply_recorded(
                            &dispatch.runner.dm,
                            &dispatch.runner.event_journal,
                            &dispatch.reply_group_id,
                            &format!("already queued {}", outcome.id),
                        );
                        return;
                    }
                    let worker_running =
                        runtime::role_is_running(dispatch.runner.app.config(), RuntimeRole::Worker)
                            .unwrap_or(false);
                    if !worker_running {
                        try_send_dm_reply_recorded(
                            &dispatch.runner.dm,
                            &dispatch.runner.event_journal,
                            &dispatch.reply_group_id,
                            "queued\nworker: offline\nstart: agentnoise worker start --tmux",
                        );
                    }
                }
                Err(error) => {
                    try_send_dm_reply_recorded(
                        &dispatch.runner.dm,
                        &dispatch.runner.event_journal,
                        &dispatch.reply_group_id,
                        &format!("Error: failed to queue job: {error:#}"),
                    );
                }
            }
        }
    }
}

fn is_bare_active_job_followup(
    event: &agentnoise::dm::MessageEvent,
    group_id: &str,
    primary_group_id: &str,
) -> bool {
    let text = event.text.trim();
    !text.is_empty()
        && !text.starts_with('/')
        && !group_id.trim().is_empty()
        && group_id != primary_group_id.trim()
}

fn should_intercept_active_followup(
    action: &RouteAction,
    event: &agentnoise::dm::MessageEvent,
    group_id: &str,
    primary_group_id: &str,
) -> bool {
    matches!(action, RouteAction::Run(_))
        && is_bare_active_job_followup(event, group_id, primary_group_id)
}

fn active_job_followup_text(job: &QueuedJob) -> String {
    let job_id = short_ref(&job.id);
    let status = match job.status {
        agentnoise::queue::QueueStatus::Queued => "queued",
        agentnoise::queue::QueueStatus::Claimed => "starting",
        agentnoise::queue::QueueStatus::Running => "running",
        agentnoise::queue::QueueStatus::Succeeded | agentnoise::queue::QueueStatus::Failed => {
            "active"
        }
    };
    format!(
        "Still working · {job_id}\nStatus: {status}\nReply after it finishes, or use /tail {job_id} /cancel {job_id}."
    )
}

fn run_inline_dm_job(runner: DmJobRunner, group_id: String, request: AgentRequest) {
    thread::spawn(move || {
        let request_for_media = request.clone();
        let result = run_dm_job_to_record(
            runner.clone(),
            group_id.clone(),
            format!("inline-{}", Uuid::new_v4().simple()),
            request,
        );
        let reply = match &result {
            Ok(record) => runner.app.render_job_record(record),
            Err(error) => format!("Error: job failed to start: {error:#}"),
        };
        if let Err(error) =
            send_dm_reply_recorded(&runner.dm, &runner.event_journal, &group_id, &reply)
        {
            eprintln!("agentnoise: failed to send job reply: {error:#}");
        }
        if let Ok(record) = result {
            upload_referenced_dm_job_media(
                &runner.app,
                &runner.dm,
                &runner.event_journal,
                &group_id,
                &request_for_media,
                &record,
            );
        }
    });
}

fn run_queued_dm_job(queue: &JobQueue, runner: DmJobRunner, job: QueuedJob) -> Result<()> {
    queue.mark_running(&job.id)?;
    let group_id = job.reply_group_id.clone();
    let result = run_dm_job_to_record(
        runner.clone(),
        group_id.clone(),
        job.id.clone(),
        job.request.clone(),
    );

    match result {
        Ok(record) => {
            let reply = runner.app.render_job_record(&record);
            match record.status {
                agentnoise::jobs::JobStatus::Succeeded => {
                    queue.mark_succeeded(
                        &job.id,
                        record.summary.as_deref().unwrap_or(""),
                        Some(&record.log_path),
                    )?;
                }
                _ => {
                    let summary = record.summary.as_deref().unwrap_or("job did not succeed");
                    queue.mark_failed(&job.id, summary, Some(&record.log_path))?;
                }
            }
            send_dm_reply_recorded(&runner.dm, &runner.event_journal, &group_id, &reply)
                .context("sending queued job reply")?;
            upload_referenced_dm_job_media(
                &runner.app,
                &runner.dm,
                &runner.event_journal,
                &group_id,
                &job.request,
                &record,
            );
        }
        Err(error) => {
            let text = format!("Error: job failed to start: {error:#}");
            queue.mark_failed(&job.id, &text, None)?;
            send_dm_reply_recorded(&runner.dm, &runner.event_journal, &group_id, &text)
                .context("sending queued job failure")?;
        }
    }
    Ok(())
}

fn run_dm_job_to_record(
    runner: DmJobRunner,
    group_id: String,
    _job_id: String,
    request: AgentRequest,
) -> Result<agentnoise::jobs::JobRecord> {
    let progress_dm = Arc::clone(&runner.dm);
    let progress_journal = Arc::clone(&runner.event_journal);
    let progress_group = group_id.clone();
    let _ = (&runner.engine, &runner.account_id_hex, &runner.handle);
    runner
        .app
        .run_request_record_with_progress(request, move |text| {
            if let Err(error) =
                send_dm_reply_recorded(&progress_dm, &progress_journal, &progress_group, &text)
            {
                eprintln!("agentnoise: failed to send progress reply: {error:#}");
            }
        })
}

fn queue_source_event_id(event: &agentnoise::dm::MessageEvent, group_id: &str) -> String {
    let event_id = event
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("local-{}", Uuid::new_v4().simple()));
    format!("{group_id}:{event_id}")
}

#[derive(Debug, Clone)]
struct AttachmentIngestResult {
    record_id: String,
    summary: String,
    reply_lines: Vec<String>,
    prompt_lines: Vec<String>,
}

impl AttachmentIngestResult {
    fn new(record_id: String, summary: String) -> Self {
        Self {
            record_id,
            summary,
            reply_lines: Vec::new(),
            prompt_lines: Vec::new(),
        }
    }

    fn reply_text(&self) -> String {
        let mut lines = vec![format!("Attachment saved: {}", self.summary)];
        if self.reply_lines.is_empty() {
            lines.push(format!("Send /attach {} for details.", self.record_id));
        } else {
            lines.extend(self.reply_lines.clone());
        }
        lines.join("\n")
    }

    fn prompt_context(&self) -> Option<String> {
        if self.prompt_lines.is_empty() {
            return None;
        }
        Some(format!(
            "Attached Dark Matter media ingested by agentnoise:\n{}\nUse these local file paths when inspecting the attached media.",
            self.prompt_lines.join("\n")
        ))
    }

    fn wiki_ingest_context(&self) -> Option<String> {
        if self.prompt_lines.is_empty() {
            return None;
        }
        Some(format!(
            "Attached Dark Matter media for the LLM Wiki ingest pipeline:\n{}\nUse the wiki File Ingestion workflow for media/files: create immutable raw metadata stubs for these local file sources, include file paths plus visible-content or file-metadata descriptions/provenance, then continue with the user's wiki request.",
            self.prompt_lines.join("\n")
        ))
    }
}

fn ingest_dm_attachments(
    app: &AgentApp,
    dm: &DmClient,
    group_id: &str,
    sender: Option<&str>,
    request: agentnoise::app::AttachmentIngestAction,
) -> AttachmentIngestResult {
    let record = request.record;
    let mut result = AttachmentIngestResult::new(
        record.id.clone(),
        attachments::render_record_summary(&record),
    );

    for (index, attachment) in record.attachments.iter().enumerate() {
        let Some(media_kind) = attachments::supported_media_kind(attachment) else {
            continue;
        };
        let media_label = media_kind.label();
        let display_name = attachment
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(media_label);

        if let Some(local_path) = attachment
            .local_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            let source_path = Path::new(local_path);
            if source_path.is_file() {
                copy_ingested_attachment(
                    app,
                    IngestCopyTarget {
                        group_id,
                        sender,
                        record_id: &record.id,
                        index,
                        attachment,
                        source_path,
                        display_name,
                        media_label,
                    },
                    &mut result,
                );
                continue;
            }
            result.reply_lines.push(format!(
                "{media_label} saved but local copy failed for {display_name}: source path is not a file: {local_path}"
            ));
            result.prompt_lines.push(format!(
                "- {media_label} {display_name}: metadata saved as {}, but source path was not readable: {local_path}",
                record.id
            ));
            if !attachments::is_downloadable_media(attachment) {
                continue;
            }
        }

        if !attachments::is_downloadable_media(attachment) {
            result.reply_lines.push(format!(
                "{media_label} saved but not downloaded: {display_name} has no complete Dark Matter media reference"
            ));
            result.prompt_lines.push(format!(
                "- {media_label} {display_name}: metadata saved as {}, but no complete downloadable media reference was present",
                record.id
            ));
            continue;
        }

        match dm.download_attachment_blocking(group_id, attachment) {
            Ok(download) => {
                let output_path = app.attachment_download_path_for_message(
                    Some(group_id),
                    sender,
                    &record.id,
                    index,
                    attachment,
                );
                match write_private_file(&output_path, &download.plaintext) {
                    Ok(()) => {
                        if let Err(error) = app.record_attachment_downloaded(
                            &record.id,
                            index,
                            &output_path,
                            Some(download.size_bytes),
                        ) {
                            eprintln!("agentnoise: failed to update attachment store: {error:#}");
                        }
                        result.reply_lines.push(format!(
                            "media ingested: {} {} -> {} ({} bytes)",
                            media_label,
                            display_name,
                            output_path.display(),
                            download.size_bytes
                        ));
                        result.prompt_lines.push(format!(
                            "- {}: {} ({} bytes)",
                            prompt_media_name(media_label, display_name),
                            output_path.display(),
                            download.size_bytes
                        ));
                    }
                    Err(error) => {
                        result.reply_lines.push(format!(
                            "{media_label} saved but local write failed for {display_name}: {error:#}"
                        ));
                        result.prompt_lines.push(format!(
                            "- {}: metadata saved as {}, but local write failed: {error:#}",
                            prompt_media_name(media_label, display_name),
                            record.id
                        ));
                    }
                }
            }
            Err(error) => {
                result.reply_lines.push(format!(
                    "{media_label} saved but download failed for {display_name}: {error:#}"
                ));
                result.prompt_lines.push(format!(
                    "- {media_label} {display_name}: metadata saved as {}, but download failed: {error:#}",
                    record.id
                ));
            }
        }
    }

    result
}

struct IngestCopyTarget<'a> {
    group_id: &'a str,
    sender: Option<&'a str>,
    record_id: &'a str,
    index: usize,
    attachment: &'a attachments::AttachmentInfo,
    source_path: &'a Path,
    display_name: &'a str,
    media_label: &'a str,
}

fn copy_ingested_attachment(
    app: &AgentApp,
    target: IngestCopyTarget<'_>,
    result: &mut AttachmentIngestResult,
) {
    let output_path = app.attachment_download_path_for_message(
        Some(target.group_id),
        target.sender,
        target.record_id,
        target.index,
        target.attachment,
    );
    match copy_private_file(target.source_path, &output_path) {
        Ok(size) => {
            if let Err(error) = app.record_attachment_downloaded(
                target.record_id,
                target.index,
                &output_path,
                Some(size),
            ) {
                eprintln!("agentnoise: failed to update attachment store: {error:#}");
            }
            result.reply_lines.push(format!(
                "media ingested: {} {} -> {} ({size} bytes)",
                target.media_label,
                target.display_name,
                output_path.display()
            ));
            result.prompt_lines.push(format!(
                "- {}: {} ({size} bytes)",
                prompt_media_name(target.media_label, target.display_name),
                output_path.display()
            ));
        }
        Err(error) => {
            result.reply_lines.push(format!(
                "{} saved but local copy failed for {}: {error:#}",
                target.media_label, target.display_name
            ));
            result.prompt_lines.push(format!(
                "- {}: metadata saved as {}, but local copy failed: {error:#}",
                prompt_media_name(target.media_label, target.display_name),
                target.record_id
            ));
        }
    }
}

fn prompt_media_name(media_label: &str, display_name: &str) -> String {
    if display_name.eq_ignore_ascii_case(media_label) {
        display_name.to_string()
    } else {
        format!("{media_label} {display_name}")
    }
}

fn action_with_attachment_context(
    action: RouteAction,
    ingest: &AttachmentIngestResult,
) -> RouteAction {
    match action {
        RouteAction::Run(mut request) => {
            let context = if looks_like_wiki_agent_prompt(&request.prompt) {
                ingest.wiki_ingest_context()
            } else {
                ingest.prompt_context()
            };
            if let Some(context) = context {
                request.prompt = format!("{}\n\n{}", request.prompt.trim_end(), context);
            }
            RouteAction::Run(request)
        }
        RouteAction::Reply(reply) => {
            RouteAction::Reply(format!("{}\n\n{}", ingest.reply_text(), reply.trim()))
        }
        action => action,
    }
}

fn looks_like_wiki_agent_prompt(prompt: &str) -> bool {
    let prompt = prompt.trim_start();
    prompt == "@wiki"
        || prompt.starts_with("@wiki ")
        || prompt == "wiki"
        || prompt.starts_with("wiki ")
}

fn upload_referenced_dm_job_media(
    app: &AgentApp,
    dm: &DmClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    request: &AgentRequest,
    record: &agentnoise::jobs::JobRecord,
) {
    if record.status != agentnoise::jobs::JobStatus::Succeeded {
        return;
    }
    let Some(summary) = record
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    else {
        return;
    };

    for path in referenced_media_paths(summary, app.config(), request)
        .into_iter()
        .take(4)
    {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("media");
        let caption = format!("agent output: {file_name}");
        if let Err(error) = dm.upload_file_blocking(group_id, &path, Some(caption)) {
            eprintln!(
                "agentnoise: failed to upload referenced media {}: {error:#}",
                path.display()
            );
            try_send_dm_reply_recorded(
                dm,
                event_journal,
                group_id,
                &format!(
                    "Warning: failed to send media {}: {error:#}",
                    path.display()
                ),
            );
        }
    }
}

fn referenced_media_paths(text: &str, config: &Config, request: &AgentRequest) -> Vec<PathBuf> {
    let Some((root, workdir)) = request_workspace_paths(config, request) else {
        return Vec::new();
    };
    let root = root.canonicalize().unwrap_or(root);
    let workdir = workdir.canonicalize().unwrap_or(workdir);
    let mut output = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for candidate in media_path_candidates(text) {
        let candidate_path = Path::new(&candidate);
        let path = if candidate_path.is_absolute() {
            candidate_path.to_path_buf()
        } else {
            workdir.join(candidate_path)
        };
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !path.is_file() || !path.starts_with(&root) {
            continue;
        }
        if is_agentnoise_attachment_path(&path) {
            continue;
        }
        if !attachments::has_supported_media_extension(&path.display().to_string()) {
            continue;
        }
        let key = path.display().to_string();
        if seen.insert(key) {
            output.push(path);
        }
    }
    output
}

fn is_agentnoise_attachment_path(path: &Path) -> bool {
    let mut components = path.components().filter_map(|component| match component {
        std::path::Component::Normal(value) => value.to_str(),
        _ => None,
    });
    while let Some(component) = components.next() {
        if component == ".agentnoise" && matches!(components.next(), Some("attachments")) {
            return true;
        }
    }
    false
}

fn request_workspace_paths(config: &Config, request: &AgentRequest) -> Option<(PathBuf, PathBuf)> {
    let root = if let Some(root) = &request.workspace_root {
        root.clone()
    } else {
        let alias = request.repo_alias.as_deref()?;
        config.repo_path(alias)?
    };
    let workdir = workspace::resolve_cwd(&root, request.cwd.as_deref()).ok()?;
    Some((root, workdir))
}

fn media_path_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for chunk in text.split(['(', ')', '<', '>', '"', '\'', '`']) {
        for token in chunk.split_whitespace() {
            let token = token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    ',' | ';' | ':' | '!' | '?' | '.' | '[' | ']' | '{' | '}' | '*'
                )
            });
            if token.is_empty() || token.contains("://") {
                continue;
            }
            if attachments::has_supported_media_extension(token) {
                candidates.push(token.to_string());
            }
        }
    }
    candidates
}

fn try_upload_dm_media(
    dm: &DmClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    request: agentnoise::app::MediaUploadAction,
) {
    let reply = match dm.upload_file_blocking(group_id, &request.path, request.caption) {
        Ok(upload) => format!(
            "uploaded and sent {}\nhash: {}\nmedia: {}",
            upload.reference.file_name, upload.reference.file_hash_hex, upload.reference.url
        ),
        Err(error) => format!("Error: upload failed: {error:#}"),
    };
    try_send_dm_reply_recorded(dm, event_journal, group_id, &reply);
}

fn try_download_dm_media(
    app: &AgentApp,
    dm: &DmClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    request: agentnoise::app::MediaDownloadAction,
) {
    let reply = match dm.download_attachment_blocking(group_id, &request.attachment) {
        Ok(download) => match write_private_file(&request.output_path, &download.plaintext) {
            Ok(()) => {
                if let Err(error) = app.record_attachment_downloaded(
                    &request.record_id,
                    request.attachment_index,
                    &request.output_path,
                    Some(download.size_bytes),
                ) {
                    eprintln!("agentnoise: failed to update attachment store: {error:#}");
                }
                format!(
                    "downloaded {}\n{} bytes\nsaved: {}",
                    download.file_name,
                    download.size_bytes,
                    request.output_path.display()
                )
            }
            Err(error) => format!("Error: saving download failed: {error:#}"),
        },
        Err(error) => format!("Error: download failed: {error:#}"),
    };
    try_send_dm_reply_recorded(dm, event_journal, group_id, &reply);
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))
}

fn copy_private_file(source: &Path, destination: &Path) -> Result<u64> {
    if source == destination {
        return fs::metadata(source)
            .map(|metadata| metadata.len())
            .with_context(|| format!("reading metadata for {}", source.display()));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let bytes = fs::copy(source, destination)
        .with_context(|| format!("copying {} to {}", source.display(), destination.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", destination.display()))?;
    }
    Ok(bytes)
}

fn send_dm_reply_recorded(
    dm: &DmClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    text: &str,
) -> Result<()> {
    const ATTEMPTS: usize = 5;

    if let Ok(mut journal) = event_journal.lock()
        && let Err(error) = journal.record_outbound_queued(group_id, text)
    {
        eprintln!("agentnoise: failed to record queued outbound event: {error:#}");
    }

    let mut last_error = None;
    for attempt in 1..=ATTEMPTS {
        match dm.send_reply_to_blocking(group_id, text) {
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
                    thread::sleep(send_retry_delay(&detail, attempt));
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

fn try_send_dm_reply_recorded(
    dm: &DmClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    text: &str,
) {
    if let Err(error) = send_dm_reply_recorded(dm, event_journal, group_id, text) {
        eprintln!("agentnoise: failed to send reply to {group_id}: {error:#}");
    }
}

fn send_retry_delay(detail: &str, attempt: usize) -> Duration {
    let attempt = attempt as u64;
    if detail.to_ascii_lowercase().contains("pending proposal") {
        return Duration::from_millis(750 * attempt * attempt);
    }
    Duration::from_millis(500 * attempt)
}

#[cfg(test)]
fn should_send_plain_final_reply(
    stream_finalized: bool,
    streamed_progress: bool,
    stream_broker_finished: bool,
) -> bool {
    !stream_finalized || !streamed_progress || !stream_broker_finished
}

fn spawn_group_join_discovery(
    tokio_handle: &tokio::runtime::Handle,
    engine: DarkmatterEngine,
    dm: Arc<DmClient>,
    account_id_hex: String,
    config_path: PathBuf,
    initial_group_ids: std::collections::HashSet<String>,
    tx: mpsc::Sender<agentnoise::dm::MessageEvent>,
) {
    let mut events = engine.runtime().subscribe();
    let mut subscribed_groups = initial_group_ids;
    tokio_handle.spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            let marmot_app::MarmotAppEvent::GroupJoined {
                account_id_hex: event_account_id,
                group_id,
                ..
            } = event
            else {
                continue;
            };
            if event_account_id != account_id_hex {
                continue;
            }
            let group_id_hex = hex::encode(group_id.as_slice());
            if !subscribed_groups.insert(group_id_hex.clone()) {
                continue;
            }
            eprintln!("agentnoise: joined group {group_id_hex} via darkmatter welcome");
            if let Err(error) = persist_discovered_group(&config_path, &group_id_hex) {
                eprintln!(
                    "agentnoise: failed to persist discovered group {group_id_hex}: {error:#}"
                );
            }
            let dm_inner = Arc::clone(&dm);
            let tx_inner = tx.clone();
            let group_id_hex_inner = group_id_hex.clone();
            tokio::spawn(async move {
                let mut subscription = match dm_inner.subscribe_group(&group_id_hex_inner).await {
                    Ok(subscription) => subscription,
                    Err(error) => {
                        eprintln!(
                            "agentnoise: failed to subscribe to {group_id_hex_inner}: {error:#}"
                        );
                        return;
                    }
                };
                for event in subscription.snapshot() {
                    if tx_inner.send(event).is_err() {
                        return;
                    }
                }
                while let Some(event) = subscription.next_message().await {
                    if tx_inner.send(event).is_err() {
                        return;
                    }
                }
            });
        }
    });
}

fn persist_discovered_group(config_path: &Path, group_id_hex: &str) -> Result<()> {
    let mut config = Config::load(config_path)?;
    config.darkmatter.add_control_group_id(group_id_hex);
    config.save(config_path)?;
    Ok(())
}

fn spawn_pairing_pin_display(config: Config, pairing: PairingRuntime) {
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
            if let Err(error) = runtime::update_pairing_pin(
                &config,
                Some(RuntimePairingPin::from_pairing_pin(&pin)),
            ) {
                eprintln!("agentnoise: failed to publish pairing PIN to runtime status: {error:#}");
            }
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
                    if let Err(error) = runtime::clear_pairing(&config) {
                        eprintln!("agentnoise: failed to clear runtime pairing status: {error:#}");
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

fn spawn_local_session_watcher_simple(
    config_path: &Path,
    config: &Config,
    dm: &Arc<DmClient>,
    event_journal: &Arc<Mutex<EventJournal>>,
) {
    if !config.local_sessions.watch {
        return;
    }
    let watch_interval = Duration::from_secs(config.local_sessions.watch_interval_seconds.max(5));
    let notify_limit = config.local_sessions.notify_limit;
    let dm = Arc::clone(dm);
    let event_journal = Arc::clone(event_journal);
    let config_path = config_path.to_path_buf();
    let initial_config = config.clone();
    thread::spawn(move || {
        let mut seen = std::collections::HashSet::<String>::new();
        loop {
            // Reload config to pick up new pairings since startup.
            let current_config = match Config::load(&config_path) {
                Ok(cfg) => cfg,
                Err(_) => initial_config.clone(),
            };
            let Some(group_id) = local_session_notification_group_from_config(&current_config)
            else {
                thread::sleep(watch_interval);
                continue;
            };
            match local_sessions::discover_local_sessions(notify_limit) {
                Ok(sessions) => {
                    let new_sessions: Vec<_> = sessions
                        .into_iter()
                        .filter(|session| seen.insert(format!("{}:{}", session.agent, session.id)))
                        .collect();
                    if !new_sessions.is_empty() {
                        let notice = local_sessions::render_new_session_notice(&new_sessions);
                        if let Err(error) = dm.send_reply_to_blocking(&group_id, &notice) {
                            eprintln!("agentnoise: failed to send local session notice: {error:#}");
                        } else if let Ok(mut journal) = event_journal.lock() {
                            let _ = journal.record_outbound(&group_id, &notice, true, None);
                        }
                    }
                }
                Err(error) => {
                    eprintln!("agentnoise: local session watch failed: {error:#}");
                }
            }
            thread::sleep(watch_interval);
        }
    });
}

fn local_session_notification_group_from_config(config: &Config) -> Option<String> {
    if !config.local_sessions.watch {
        return None;
    }
    if config.darkmatter.allowed_senders.is_empty() {
        return None;
    }
    let primary = config.darkmatter.group_id.trim();
    if primary.is_empty() {
        return None;
    }
    Some(primary.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_session_notifications_are_opt_in_and_primary_chat_only() {
        let mut config = Config::template();
        config.darkmatter.group_id = "group-a".to_string();
        config
            .darkmatter
            .allowed_senders
            .push("npub1pairedphone".to_string());

        assert_eq!(local_session_notification_group_from_config(&config), None);

        config.local_sessions.watch = true;
        assert_eq!(
            local_session_notification_group_from_config(&config),
            Some("group-a".to_string())
        );

        config.darkmatter.group_id.clear();
        assert_eq!(local_session_notification_group_from_config(&config), None);
    }

    fn dm_message(sender: &str, id: &str, text: &str) -> agentnoise::dm::MessageEvent {
        agentnoise::dm::MessageEvent {
            group_id: Some("worker".to_string()),
            sender: Some(sender.to_string()),
            text: text.to_string(),
            unsupported: None,
            id: Some(id.to_string()),
            trigger: None,
            is_initial: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn bare_work_chat_followup_with_active_job_is_intercepted() {
        let event = dm_message("phone", "m1", "can you keep going?");
        assert!(is_bare_active_job_followup(&event, "worker", "inbox"));

        let inbox = dm_message("phone", "m2", "can you keep going?");
        assert!(!is_bare_active_job_followup(&inbox, "inbox", "inbox"));

        let slash = dm_message("phone", "m3", "/tail an-123");
        assert!(!is_bare_active_job_followup(&slash, "worker", "inbox"));

        assert!(should_intercept_active_followup(
            &RouteAction::Run(AgentRequest::prompt(AgentKind::Codex, "Give me list")),
            &event,
            "worker",
            "inbox"
        ));
        assert!(!should_intercept_active_followup(
            &RouteAction::Reply("Queued.".to_string()),
            &event,
            "worker",
            "inbox"
        ));
        assert!(!should_intercept_active_followup(
            &RouteAction::Ignore,
            &event,
            "worker",
            "inbox"
        ));
    }

    #[test]
    fn active_job_followup_text_does_not_start_second_job_silently() {
        let mut request = AgentRequest::prompt(AgentKind::Codex, "work");
        request.repo_alias = Some("sandbox".to_string());
        let job = QueuedJob {
            id: "q-abcdef12".to_string(),
            status: agentnoise::queue::QueueStatus::Running,
            request,
            source_group_id: "inbox".to_string(),
            reply_group_id: "worker".to_string(),
            source_event_id: "event-1".to_string(),
            created_at: "now".to_string(),
            claimed_by: None,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            log_path: None,
            summary: None,
            error: None,
        };

        let text = active_job_followup_text(&job);

        assert!(text.contains("Still working · q-abcde"));
        assert!(text.contains("Reply after it finishes"));
        assert!(text.contains("/tail q-abcde"));
        assert!(text.contains("/cancel q-abcde"));
    }

    #[test]
    fn attachment_context_is_added_to_captioned_run() {
        let mut ingest = AttachmentIngestResult::new("att-123".to_string(), "1 file".to_string());
        ingest.prompt_lines.push(
            "- image shot.png: /repo/.agentnoise/attachments/att-123/01-shot.png (12 bytes)"
                .to_string(),
        );

        let action = action_with_attachment_context(
            RouteAction::Run(AgentRequest::prompt(AgentKind::Codex, "describe this")),
            &ingest,
        );

        assert!(matches!(
            action,
            RouteAction::Run(request)
                if request.prompt.contains("describe this")
                    && request.prompt.contains("Attached Dark Matter media ingested")
                    && request.prompt.contains("01-shot.png")
        ));
    }

    #[test]
    fn wiki_attachment_context_uses_ingest_pipeline_language() {
        let mut ingest = AttachmentIngestResult::new("att-123".to_string(), "1 file".to_string());
        ingest.prompt_lines.push(
            "- PDF report.pdf: /repo/.agentnoise/attachments/att-123/01-report.pdf (12 bytes)"
                .to_string(),
        );

        let action = action_with_attachment_context(
            RouteAction::Run(AgentRequest::prompt(
                AgentKind::Codex,
                "@wiki ingest report",
            )),
            &ingest,
        );

        assert!(matches!(
            action,
            RouteAction::Run(request)
                if request.prompt.contains("LLM Wiki ingest pipeline")
                    && request.prompt.contains("File Ingestion workflow")
                    && request.prompt.contains("01-report.pdf")
        ));
    }

    #[test]
    fn referenced_media_paths_stay_inside_workspace_and_skip_ingested_inputs() {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let output = repo.path().join("out.png");
        let ingested = repo
            .path()
            .join(".agentnoise")
            .join("attachments")
            .join("att-1")
            .join("01-input.png");
        let outside_file = outside.path().join("outside.png");
        std::fs::create_dir_all(ingested.parent().unwrap()).unwrap();
        std::fs::write(&output, b"png").unwrap();
        std::fs::write(&ingested, b"png").unwrap();
        std::fs::write(&outside_file, b"png").unwrap();

        let mut config = Config::template();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        let request = AgentRequest::new(AgentKind::Codex, "work", "draw");
        let text = format!(
            "Created out.png and {} and {}",
            ingested.display(),
            outside_file.display()
        );

        let paths = referenced_media_paths(&text, &config, &request);

        assert_eq!(paths, vec![output.canonicalize().unwrap()]);
    }

    #[test]
    fn plain_final_reply_is_only_stream_fallback() {
        assert!(!should_send_plain_final_reply(true, true, true));
        assert!(should_send_plain_final_reply(true, false, true));
        assert!(should_send_plain_final_reply(false, true, true));
        assert!(should_send_plain_final_reply(true, true, false));
    }
}
