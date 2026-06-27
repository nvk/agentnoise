use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use agentnoise::app::{AgentApp, NewSessionRequest, RouteAction};
use agentnoise::attachments;
use agentnoise::auth::PairingGate;
use agentnoise::config::{Config, RunnerLauncher};
use agentnoise::desktop_alert;
use agentnoise::doctor::render_doctor;
use agentnoise::events::EventJournal;
use agentnoise::identity;
use agentnoise::launchd;
use agentnoise::local_sessions::{self, LocalAgentSession};
use agentnoise::queue::{JobQueue, QueueStatus, QueuedJob};
use agentnoise::runner::{AgentKind, AgentRequest};
use agentnoise::runtime::{
    self, AcquireMode, EngineGuard, RuntimePairingInfo, RuntimePairingPin, RuntimeRole,
};
use agentnoise::secrets;
use agentnoise::service::{self, ServiceTarget};
use agentnoise::setup::{self, SetupOptions, SetupResult};
use agentnoise::subscriptions::{self, SubscriptionRegistry};
use agentnoise::text::{compact_text, compact_timestamp, short_ref};
use agentnoise::whitenoise_cli::{self, WhitenoiseInstall};
use agentnoise::wn::{MessageEvent, WnClient};
use agentnoise::workspace;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use zeroize::Zeroize;

const FIRST_PAIRING_SUBSCRIBE_LIMIT: u32 = 20;
const SUBSCRIPTION_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const SUBSCRIPTION_STALE_IDLE: Duration = Duration::from_secs(90);
const SUBSCRIPTION_RECONCILE_LIMIT: u32 = 20;

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
#[command(about = "Chat with local coding agents through White Noise")]
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
    #[command(about = "Run only the White Noise transport/queue listener")]
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
    #[arg(
        long,
        help = "Use raw Codex/Claude/Hermes CLIs directly. This is the default for new configs."
    )]
    direct_agents: bool,
    #[arg(
        long,
        alias = "secure",
        help = "Use bondage profiles instead of raw agent CLIs"
    )]
    bondage: bool,
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
    #[arg(
        long,
        help = "Use raw Codex/Claude/Hermes CLIs directly. This is the default for new configs."
    )]
    direct_agents: bool,
    #[arg(
        long,
        alias = "secure",
        help = "Use bondage profiles instead of raw agent CLIs"
    )]
    bondage: bool,
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
        help = "Use raw Codex/Claude/Hermes CLIs directly. This is the default for new configs."
    )]
    direct_agents: bool,
    #[arg(
        long,
        alias = "secure",
        help = "Use bondage profiles instead of raw agent CLIs"
    )]
    bondage: bool,
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
    #[command(about = "Listen to White Noise and enqueue agent jobs")]
    Run(TransportRunArgs),
    #[command(about = "Show transport role status")]
    Status,
}

#[derive(Debug, Args)]
struct TransportRunArgs {
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
    #[arg(long, help = "Do not start wn daemon automatically")]
    no_daemon: bool,
    #[arg(
        long,
        help = "Development only: use a plaintext throwaway nsec under the agentnoise data dir instead of the OS keychain"
    )]
    dev_burner_nsec: bool,
    #[arg(
        long,
        help = "Use raw Codex/Claude/Hermes CLIs directly. This is the default for new configs."
    )]
    direct_agents: bool,
    #[arg(
        long,
        alias = "secure",
        help = "Use bondage profiles instead of raw agent CLIs"
    )]
    bondage: bool,
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
        #[arg(
            long,
            help = "Text that must appear in at least one observed reply; repeatable"
        )]
        expect: Vec<String>,
        #[arg(long, default_value_t = 1)]
        min_replies: usize,
        #[arg(long, help = "Require a final job reply, not only the initial ack")]
        require_job_final: bool,
        #[arg(
            long,
            help = "Use the configured/default White Noise daemon instead of starting an isolated fake-phone daemon"
        )]
        shared_daemon: bool,
        #[arg(required = true, trailing_var_arg = true)]
        message: Vec<String>,
    },
    #[command(about = "Open an interactive fake-phone terminal UI")]
    Tui {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        pin: Option<String>,
        #[arg(long, default_value = "agentnoise fake phone")]
        group_name: String,
        #[arg(
            long,
            help = "Use the configured/default White Noise daemon instead of starting an isolated fake-phone daemon"
        )]
        shared_daemon: bool,
        #[arg(
            long,
            help = "Do not auto-follow agentnoise whitenoise://chat handoff links"
        )]
        no_follow_handoffs: bool,
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
            let launcher = launcher_from_flags(args.direct_agents, args.bondage)?
                .unwrap_or(RunnerLauncher::Direct);
            Config::write_template(&config_path, args.force, launcher)?;
            println!("wrote {}", config_path.display());
            println!("agent launcher: {launcher}");
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
                    launcher: launcher_from_flags(args.direct_agents, args.bondage)?,
                    start_daemon: true,
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
                RouteAction::IngestAttachments(request) => {
                    println!(
                        "Attachment saved: {}\nRun this from the live listener for automatic image ingest.",
                        attachments::render_record_summary(&request.record)
                    );
                }
                RouteAction::NewSession(request) => {
                    println!("{}", request.created_text());
                    println!("New chat: {}", request.group_name());
                    println!("{}", request.ready_text());
                    println!(
                        "Note: `agentnoise handle` does not create the White Noise chat; run this from the live listener for real delivery."
                    );
                }
                RouteAction::ResumeSession(request) => {
                    println!("{}", request.reply_text);
                    println!("Target chat: {}", request.group_id);
                    println!("{}", request.target_text);
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
            start_command(&config_path, args)?;
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
                let config = Config::load_or_template(&config_path)?;
                let removed = launchd::uninstall(&config, unload)?;
                if removed {
                    println!("removed {}", launchd::plist_path(&config).display());
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
                    println!("{}", identity::render_qr(&payload.npub)?);
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

fn launcher_from_flags(direct_agents: bool, bondage: bool) -> Result<Option<RunnerLauncher>> {
    if direct_agents && bondage {
        bail!("--direct-agents and --bondage cannot be combined");
    }
    if bondage {
        Ok(Some(RunnerLauncher::Bondage))
    } else if direct_agents {
        Ok(Some(RunnerLauncher::Direct))
    } else {
        Ok(None)
    }
}

fn start_command(config_path: &Path, args: StartArgs) -> Result<()> {
    up(
        config_path,
        UpArgs {
            phone: args.phone,
            name: args.name,
            group_name: args.group_name,
            group: args.group,
            relays: args.relays,
            no_listen: args.no_listen,
            no_daemon: args.no_daemon,
            dev_burner_nsec: args.dev_burner_nsec,
            direct_agents: args.direct_agents,
            bondage: args.bondage,
            ssh: args.ssh,
        },
    )
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
    println!("agent launcher: {}", result.launcher);
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
    for warning in &result.warnings {
        println!("warning: {warning}");
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
            dev_burner_nsec: args.dev_burner_nsec,
            launcher: launcher_from_flags(args.direct_agents, args.bondage)?,
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
            phone: None,
            name: None,
            group_name: setup::DEFAULT_GROUP_NAME.to_string(),
            group: None,
            relays: Vec::new(),
            no_listen: false,
            // setup() already performed daemon startup/login/profile repair for `up`.
            no_daemon: true,
            dev_burner_nsec: false,
            direct_agents: false,
            bondage: false,
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
        || args.dev_burner_nsec
        || args.direct_agents
        || args.bondage
        || args.ssh
        || !config_path.exists()
    {
        return Ok(false);
    }

    let config = Config::load(config_path)?;
    Ok(runtime::engine_is_running(&config)?
        || runtime::role_is_running(&config, RuntimeRole::Transport)?)
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
            expect,
            min_replies,
            require_job_final,
            shared_daemon,
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
                    shared_daemon,
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
        FakePhoneCommand::Tui {
            root,
            pin,
            group_name,
            shared_daemon,
            no_follow_handoffs,
        } => {
            let root = root.unwrap_or_else(|| config.resolved_data_dir().join("fake-phone"));
            agentnoise::fake_phone::terminal(
                &config,
                agentnoise::fake_phone::FakePhoneTerminal {
                    root,
                    pin,
                    group_name,
                    shared_daemon,
                    follow_handoffs: !no_follow_handoffs,
                },
            )?;
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
        config.whitenoise.add_control_group_id(group);
        config.save(config_path)?;
    }
    let reset_wn_bin = reset_managed_wn_bin_if_needed(config_path, &mut config)?;
    wait_for_inline_listener_before_transport(config_path, &config)?;

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
    let _guard = guard;
    let engine_guard = runtime::acquire_engine(config_path, &config, AcquireMode::Try)?
        .ok_or_else(|| anyhow::anyhow!("inline listener started while transport was starting"))?;

    let _daemon = if args.no_daemon {
        None
    } else if reset_wn_bin {
        eprintln!(
            "agentnoise: migrated managed White Noise CLI path; restarting White Noise daemon"
        );
        whitenoise_cli::restart_daemon(&config.whitenoise)?;
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
        engine_guard.update_status(config_path, &config, Some(pairing))?;
    }
    let _engine_guard = engine_guard;
    run_listener(config_path, config, pairing, ListenerExecution::Queue)
}

fn reset_managed_wn_bin_if_needed(config_path: &Path, config: &mut Config) -> Result<bool> {
    if !whitenoise_cli::should_reset_wn_bin_to_default(&config.whitenoise.wn_bin) {
        return Ok(false);
    }
    let previous = std::mem::replace(&mut config.whitenoise.wn_bin, "wn".to_string());
    config.save(config_path)?;
    eprintln!(
        "agentnoise: reset managed White Noise CLI path from {} to packaged default",
        previous
    );
    Ok(true)
}

fn wait_for_inline_listener_before_transport(config_path: &Path, config: &Config) -> Result<()> {
    if !runtime::engine_is_running(config)? {
        return Ok(());
    }
    if runtime::stdio_is_interactive() {
        runtime::attach_ui(config_path, config)?;
        return Ok(());
    }

    let mut last_notice = Instant::now() - Duration::from_secs(30);
    while runtime::engine_is_running(config)? {
        if last_notice.elapsed() >= Duration::from_secs(30) {
            match runtime::engine_lock_owner(config)? {
                Some(pid) => eprintln!(
                    "agentnoise: inline listener is running as pid {pid}; transport startup is waiting for it to exit"
                ),
                None => eprintln!(
                    "agentnoise: inline listener is running; transport startup is waiting for it to exit"
                ),
            }
            last_notice = Instant::now();
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
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
        return start_worker_tmux(config_path, &config, args.poll_seconds);
    }

    let guard = runtime::acquire_role(config_path, &config, RuntimeRole::Worker, AcquireMode::Try)?;
    let Some(_guard) = guard else {
        match runtime::role_lock_owner(&config, RuntimeRole::Worker)? {
            Some(pid) => println!("agentnoise worker already running as pid {pid}"),
            None => println!("agentnoise worker already running"),
        }
        return Ok(());
    };

    if whitenoise_cli::ensure_login_from_configured_nsec(&config.whitenoise)? {
        eprintln!("agentnoise: restored White Noise login from configured nsec");
    }
    let queue = JobQueue::open(config.resolved_queue_path())?;
    let app = Arc::new(AgentApp::from_config_path(config_path)?);
    let wn = Arc::new(WnClient::new(app.config().whitenoise.clone()));
    let event_journal = Arc::new(Mutex::new(EventJournal::open(
        &app.config().resolved_event_log_path(),
    )?));
    let worker_id = format!("worker:{}", std::process::id());
    let idle_delay = Duration::from_secs(args.poll_seconds.max(1));

    println!("agentnoise worker running");
    loop {
        match queue.claim_next(&worker_id) {
            Err(error) => {
                eprintln!("agentnoise worker: queue claim failed: {error:#}");
                if args.once {
                    return Err(error);
                }
                thread::sleep(idle_delay);
            }
            Ok(Some(job)) => {
                let job_id = job.id.clone();
                if let Err(error) = run_queued_job(
                    &queue,
                    Arc::clone(&app),
                    Arc::clone(&wn),
                    Arc::clone(&event_journal),
                    job,
                ) {
                    eprintln!("agentnoise worker: job {job_id} failed: {error:#}");
                    let failure = format!("worker error: {error:#}");
                    if let Err(mark_error) = queue.mark_failed(&job_id, &failure, None) {
                        eprintln!(
                            "agentnoise worker: failed to mark {job_id} failed: {mark_error:#}"
                        );
                    }
                }
            }
            Ok(None) if args.once => {
                println!("agentnoise worker: no queued jobs");
                return Ok(());
            }
            Ok(None) => thread::sleep(idle_delay),
        }
        if args.once {
            return Ok(());
        }
    }
}

fn start_worker_tmux(config_path: &Path, config: &Config, poll_seconds: u64) -> Result<()> {
    if runtime::role_is_running(config, RuntimeRole::Worker)? {
        match runtime::role_lock_owner(config, RuntimeRole::Worker)? {
            Some(pid) => println!("agentnoise worker already running as pid {pid}"),
            None => println!("agentnoise worker already running"),
        }
        return Ok(());
    }
    ensure_tmux_available()?;

    let exe = std::env::current_exe().context("resolving current executable")?;
    let session = worker_tmux_session_name(config);
    if tmux_session_exists(&session)? {
        println!("agentnoise worker tmux session: {session}");
        return Ok(());
    }
    let command = supervised_worker_shell_command(&exe, config_path, poll_seconds.max(1));
    let status = ProcessCommand::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&session)
        .arg("/bin/sh")
        .arg("-lc")
        .arg(command)
        .status()
        .context("starting supervised tmux worker session")?;
    if !status.success() {
        bail!("tmux new-session exited with {status}");
    }
    println!("agentnoise worker tmux session: {session} (supervised)");
    Ok(())
}

fn worker_tmux_session_name(config: &Config) -> String {
    config
        .instance
        .as_deref()
        .map(|instance| format!("agentnoise-worker-{instance}"))
        .unwrap_or_else(|| "agentnoise-worker".to_string())
}

fn tmux_session_exists(session: &str) -> Result<bool> {
    let status = ProcessCommand::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(session)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("checking tmux worker session")?;
    Ok(status.success())
}

fn supervised_worker_shell_command(exe: &Path, config_path: &Path, poll_seconds: u64) -> String {
    format!(
        "while true; do {exe} --config {config} worker start --poll-seconds {poll}; status=$?; echo \"agentnoise worker exited with status $status; restarting in 5s\" >&2; sleep 5; done",
        exe = shell_quote(&exe.display().to_string()),
        config = shell_quote(&config_path.display().to_string()),
        poll = poll_seconds.max(1),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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

fn run_queued_job(
    queue: &JobQueue,
    app: Arc<AgentApp>,
    wn: Arc<WnClient>,
    event_journal: Arc<Mutex<EventJournal>>,
    job: QueuedJob,
) -> Result<()> {
    queue.mark_running(&job.id)?;
    let group_id = job.reply_group_id.clone();
    let request = job.request.clone();
    let progress_wn = Arc::clone(&wn);
    let progress_journal = Arc::clone(&event_journal);
    let progress_group = group_id.clone();
    let result = app.run_request_record_with_progress(request.clone(), move |text| {
        if let Err(error) =
            send_reply_recorded(&progress_wn, &progress_journal, &progress_group, &text)
        {
            eprintln!("agentnoise: failed to send progress reply: {error:#}");
        }
    });

    match result {
        Ok(record) => {
            let reply = app.render_job_record(&record);
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
            if let Err(error) = send_reply_recorded(&wn, &event_journal, &group_id, &reply) {
                eprintln!("agentnoise worker: failed to send queued job reply: {error:#}");
            }
            upload_referenced_job_media(&app, &wn, &event_journal, &group_id, &request, &record);
        }
        Err(error) => {
            let text = format!("Error: job failed to start: {error:#}");
            queue.mark_failed(&job.id, &text, None)?;
            if let Err(send_error) = send_reply_recorded(&wn, &event_journal, &group_id, &text) {
                eprintln!("agentnoise worker: failed to send queued job failure: {send_error:#}");
            }
        }
    }
    Ok(())
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
        || args.dev_burner_nsec
        || args.ssh
        || !config_path.exists()
    {
        return Ok(());
    }

    let config = Config::load(config_path)?;
    let mut last_notice = Instant::now() - Duration::from_secs(30);
    while runtime::engine_is_running(&config)?
        || runtime::role_is_running(&config, RuntimeRole::Transport)?
    {
        if last_notice.elapsed() >= Duration::from_secs(30) {
            if let Some(pid) = runtime::engine_lock_owner(&config)? {
                eprintln!(
                    "agentnoise: another inline listener is running as pid {pid}; service startup is waiting for it to exit"
                );
            } else if let Some(pid) = runtime::role_lock_owner(&config, RuntimeRole::Transport)? {
                eprintln!(
                    "agentnoise: transport is running as pid {pid}; service startup is waiting for it to exit"
                );
            } else {
                eprintln!(
                    "agentnoise: another listener is running; service startup is waiting for it to exit"
                );
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

fn discover_group(config: &mut Config, config_path: &Path) -> Result<GroupDiscovery> {
    let groups = whitenoise_cli::list_groups(&config.whitenoise)?;
    if groups.is_empty() {
        return Ok(GroupDiscovery::NeedsPairing);
    }

    whitenoise_cli::accept_pending_groups(&config.whitenoise, &groups)?;
    config
        .whitenoise
        .set_control_group_ids(groups.into_iter().map(|group| group.group_id));
    config.save(config_path)?;
    Ok(GroupDiscovery::Ready)
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
    run_listener(config_path, config, pairing, execution)
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
    run_listener(config_path, config, pairing, execution)
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
        current_pin: None,
    })
}

fn run_listener(
    config_path: &Path,
    mut config: Config,
    pairing: Option<PairingRuntime>,
    execution: ListenerExecution,
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
    reconcile_discovered_groups(config_path, &mut config);
    if let Some(pairing) = pairing.clone() {
        spawn_pairing_pin_display(config.clone(), pairing);
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
    listen(config_path, app, wn, execution)
}

fn reconcile_discovered_groups(config_path: &Path, config: &mut Config) {
    let groups = match whitenoise_cli::list_groups(&config.whitenoise) {
        Ok(groups) => groups,
        Err(error) => {
            eprintln!("agentnoise: group discovery failed: {error:#}");
            return;
        }
    };
    if groups.is_empty() {
        return;
    }
    if let Err(error) = whitenoise_cli::accept_pending_groups(&config.whitenoise, &groups) {
        eprintln!("agentnoise: failed to accept pending White Noise group(s): {error:#}");
    }

    let active_groups = groups
        .into_iter()
        .map(|group| group.group_id)
        .collect::<Vec<_>>();
    let previous_groups = config.whitenoise.control_group_ids();
    if active_groups == previous_groups {
        return;
    }

    config.whitenoise.set_control_group_ids(active_groups);
    match config.save(config_path) {
        Ok(()) => eprintln!(
            "agentnoise: reconciled White Noise control chats: {} active",
            config.whitenoise.control_group_ids().len()
        ),
        Err(error) => eprintln!("agentnoise: failed to save reconciled control chats: {error:#}"),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageSource {
    Stream,
    Reconciliation,
}

enum StreamItem {
    Event {
        event: MessageEvent,
        source: MessageSource,
    },
    StreamJson {
        group_id: String,
    },
    StreamError {
        group_id: String,
        message: String,
    },
    Reconcile {
        group_id: String,
    },
    PolledMessages {
        group_id: String,
        messages: Vec<MessageEvent>,
    },
    ReconcileError {
        group_id: String,
        message: String,
    },
    RestartSubscription {
        group_id: String,
    },
    Exited {
        group_id: String,
        status: ExitStatus,
    },
    Discovered(Vec<String>),
    DiscoveryError(String),
    LocalSessionsChanged(Vec<LocalAgentSession>),
    LocalSessionsWatchError(String),
}

#[derive(Clone)]
struct SubscriptionStateHandle {
    registry: Arc<Mutex<SubscriptionRegistry>>,
    snapshot_path: PathBuf,
}

impl SubscriptionStateHandle {
    fn new(snapshot_path: PathBuf) -> Self {
        Self {
            registry: Arc::new(Mutex::new(SubscriptionRegistry::default())),
            snapshot_path,
        }
    }
}

fn listen(
    config_path: &Path,
    app: Arc<AgentApp>,
    wn: Arc<WnClient>,
    execution: ListenerExecution,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let subscriptions = SubscriptionStateHandle::new(app.config().resolved_subscriptions_path());
    let event_journal = Arc::new(Mutex::new(EventJournal::open(
        &app.config().resolved_event_log_path(),
    )?));
    let job_queue = if execution == ListenerExecution::Queue {
        Some(JobQueue::open(app.config().resolved_queue_path())?)
    } else {
        None
    };

    let initial_groups = initial_group_ids(&wn);
    let subscribe_limit = listener_subscribe_limit(app.config());
    let mut startup_hello_sent = HashSet::new();
    if initial_groups.is_empty() {
        println!("agentnoise waiting for White Noise control chat");
        println!("agentnoise will keep discovering chats until the phone-created chat appears");
    } else {
        for group_id in initial_groups {
            subscribe_group_if_needed(
                Arc::clone(&wn),
                subscriptions.clone(),
                tx.clone(),
                &group_id,
                subscribe_limit,
            )?;
            send_startup_hello_if_needed(
                app.config(),
                &wn,
                &event_journal,
                &mut startup_hello_sent,
                &group_id,
            );
        }
    }
    spawn_group_discovery(Arc::clone(&wn), tx.clone());
    spawn_subscription_watchdog(Arc::clone(&subscriptions.registry), tx.clone());
    spawn_local_session_watcher(app.config(), tx.clone());
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
                        subscriptions.clone(),
                        tx.clone(),
                        &group_id,
                        subscribe_limit,
                    ) {
                        eprintln!("agentnoise: failed to subscribe to {group_id}: {error:#}");
                        continue;
                    }
                    send_startup_hello_if_needed(
                        app.config(),
                        &wn,
                        &event_journal,
                        &mut startup_hello_sent,
                        &group_id,
                    );
                }
            }
            StreamItem::DiscoveryError(message) => {
                eprintln!("agentnoise: group discovery failed: {message}");
            }
            StreamItem::LocalSessionsChanged(sessions) => {
                match local_session_notification_group(config_path, app.config()) {
                    Ok(Some(group_id)) => {
                        let notice = local_sessions::render_new_session_notice(&sessions);
                        try_send_reply_recorded(&wn, &event_journal, &group_id, &notice);
                    }
                    Ok(None) => {
                        eprintln!(
                            "agentnoise: local sessions changed, but no paired primary chat is configured"
                        );
                    }
                    Err(error) => {
                        eprintln!(
                            "agentnoise: failed to load local session notification config: {error:#}"
                        );
                    }
                }
            }
            StreamItem::LocalSessionsWatchError(message) => {
                eprintln!("agentnoise: local session watch failed: {message}");
            }
            StreamItem::StreamJson { group_id } => {
                if let Ok(mut registry) = subscriptions.registry.lock() {
                    registry.mark_json(&group_id);
                    save_subscriptions_snapshot(&subscriptions, &registry);
                }
            }
            StreamItem::StreamError { group_id, message } => {
                if let Ok(mut registry) = subscriptions.registry.lock() {
                    registry.mark_error(&group_id, &message);
                    save_subscriptions_snapshot(&subscriptions, &registry);
                }
                eprintln!("agentnoise: wn stream error for {group_id}: {message}");
            }
            StreamItem::Reconcile { group_id } => {
                let should_poll = {
                    let mut registry = subscriptions
                        .registry
                        .lock()
                        .map_err(|_| anyhow::anyhow!("subscription registry lock poisoned"))?;
                    let should_poll = registry.mark_poll_start(&group_id);
                    save_subscriptions_snapshot(&subscriptions, &registry);
                    should_poll
                };
                if should_poll {
                    spawn_group_reconciliation(Arc::clone(&wn), tx.clone(), group_id);
                }
            }
            StreamItem::PolledMessages { group_id, messages } => {
                let previous_latest = subscriptions
                    .registry
                    .lock()
                    .ok()
                    .and_then(|registry| registry.latest_polled_message_id(&group_id));
                let recovered_events =
                    reconciled_events_after(&messages, previous_latest.as_deref());
                let recovered_unseen = {
                    let journal = event_journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("event journal lock poisoned"))?;
                    recovered_events
                        .iter()
                        .filter(|event| {
                            event.group_id.as_deref().is_some_and(|group| {
                                !journal.already_seen(group, event.id.as_deref())
                            })
                        })
                        .count()
                };
                if let Ok(mut registry) = subscriptions.registry.lock() {
                    registry.mark_poll(&group_id, &messages);
                    if previous_latest.is_some() {
                        registry.mark_recovered(&group_id, recovered_unseen);
                    }
                    for stale_group in registry.stale_running_groups(SUBSCRIPTION_STALE_IDLE) {
                        let pid = registry.pid(&stale_group);
                        registry.mark_stale(&stale_group);
                        if let Some(pid) = pid {
                            terminate_subscription_process(pid);
                        } else {
                            schedule_subscription_restart(tx.clone(), stale_group.clone(), 1);
                        }
                    }
                    save_subscriptions_snapshot(&subscriptions, &registry);
                }
                if previous_latest.is_some() {
                    for event in recovered_events {
                        if tx
                            .send(StreamItem::Event {
                                event,
                                source: MessageSource::Reconciliation,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            StreamItem::ReconcileError { group_id, message } => {
                if let Ok(mut registry) = subscriptions.registry.lock() {
                    registry.mark_poll_error(&group_id, &message);
                    save_subscriptions_snapshot(&subscriptions, &registry);
                }
                eprintln!("agentnoise: wn reconciliation failed for {group_id}: {message}");
            }
            StreamItem::RestartSubscription { group_id } => {
                if let Err(error) = subscribe_group_if_needed(
                    Arc::clone(&wn),
                    subscriptions.clone(),
                    tx.clone(),
                    &group_id,
                    subscribe_limit,
                ) {
                    if let Ok(mut registry) = subscriptions.registry.lock() {
                        registry.mark_failed(&group_id, &format!("{error:#}"));
                        save_subscriptions_snapshot(&subscriptions, &registry);
                    }
                    eprintln!(
                        "agentnoise: failed to restart subscription for {group_id}: {error:#}"
                    );
                    schedule_subscription_restart(tx.clone(), group_id, 2);
                }
            }
            StreamItem::Exited { group_id, status } => {
                let restart_count = {
                    let mut registry = subscriptions
                        .registry
                        .lock()
                        .map_err(|_| anyhow::anyhow!("subscription registry lock poisoned"))?;
                    let restart_count = registry.mark_exit(&group_id, &status.to_string());
                    save_subscriptions_snapshot(&subscriptions, &registry);
                    restart_count
                };
                if !status.success() {
                    eprintln!("agentnoise: wn subscribe for {group_id} exited with {status}");
                }
                schedule_subscription_restart(tx.clone(), group_id, restart_count);
            }
            StreamItem::Event { event, source } => {
                let Some(group_id) = event.group_id.as_deref() else {
                    eprintln!("agentnoise: ignored message without White Noise group id");
                    continue;
                };
                if source == MessageSource::Stream
                    && let Ok(mut registry) = subscriptions.registry.lock()
                {
                    registry.mark_event(&event);
                    save_subscriptions_snapshot(&subscriptions, &registry);
                }
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
                if let Ok(mut registry) = subscriptions.registry.lock() {
                    registry.mark_journaled(&event);
                    save_subscriptions_snapshot(&subscriptions, &registry);
                }
                let process_initial_pairing = event.unsupported.is_none()
                    && app.accepts_current_pairing_pin(event.sender.as_deref(), &event.text);
                if ignore_initial && event.is_initial && !process_initial_pairing {
                    match app.route_initial_history_event(&event)? {
                        RouteAction::Ignore => {}
                        RouteAction::Reply(reply) => {
                            try_send_reply_recorded(&wn, &event_journal, group_id, &reply);
                        }
                        RouteAction::IngestAttachments(_)
                        | RouteAction::NewSession(_)
                        | RouteAction::ResumeSession(_)
                        | RouteAction::DownloadMedia(_)
                        | RouteAction::Run(_) => {}
                    }
                    continue;
                }

                if event.attachments.is_empty()
                    && let Some(message) = event.unsupported.as_deref()
                {
                    let action = app.route_unsupported_message(event.sender.as_deref(), message)?;
                    match action {
                        RouteAction::Ignore => {}
                        RouteAction::Reply(reply) => {
                            try_send_reply_recorded(&wn, &event_journal, group_id, &reply);
                        }
                        RouteAction::IngestAttachments(_)
                        | RouteAction::NewSession(_)
                        | RouteAction::ResumeSession(_)
                        | RouteAction::DownloadMedia(_)
                        | RouteAction::Run(_) => {}
                    }
                    continue;
                }

                let mut attachment_ingest = None;
                if !event.attachments.is_empty() {
                    match app.route_unsupported_event(&event)? {
                        RouteAction::Ignore => continue,
                        RouteAction::Reply(reply) => {
                            try_send_reply_recorded(&wn, &event_journal, group_id, &reply);
                            continue;
                        }
                        RouteAction::IngestAttachments(request) => {
                            let ingest = ingest_wn_attachments(
                                &app,
                                &wn,
                                group_id,
                                event.sender.as_deref(),
                                request,
                            );
                            if event.text.trim().is_empty() {
                                try_send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &ingest.reply_text(),
                                );
                                continue;
                            }
                            attachment_ingest = Some(ingest);
                        }
                        RouteAction::NewSession(_)
                        | RouteAction::ResumeSession(_)
                        | RouteAction::DownloadMedia(_)
                        | RouteAction::Run(_) => {}
                    }
                }

                let mut action = app.route_message(
                    event.group_id.as_deref(),
                    event.sender.as_deref(),
                    &event.text,
                )?;
                if let Some(ingest) = &attachment_ingest {
                    action = action_with_attachment_context(action, ingest);
                }
                if let Some(queue) = job_queue.as_ref()
                    && should_intercept_active_followup(
                        &action,
                        &event,
                        group_id,
                        &app.config().whitenoise.group_id,
                    )
                {
                    match queue.active_for_reply_group(group_id) {
                        Ok(Some(job)) => {
                            try_send_reply_recorded(
                                &wn,
                                &event_journal,
                                group_id,
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
                        try_send_reply_recorded(&wn, &event_journal, group_id, &reply);
                    }
                    RouteAction::IngestAttachments(request) => {
                        let ingest = ingest_wn_attachments(
                            &app,
                            &wn,
                            group_id,
                            event.sender.as_deref(),
                            request,
                        );
                        try_send_reply_recorded(
                            &wn,
                            &event_journal,
                            group_id,
                            &ingest.reply_text(),
                        );
                    }
                    RouteAction::NewSession(request) => {
                        match create_parallel_session(
                            config_path,
                            Arc::clone(&app),
                            Arc::clone(&wn),
                            subscriptions.clone(),
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
                                    Ok(()) => try_send_reply_recorded(
                                        &wn,
                                        &event_journal,
                                        group_id,
                                        &request.created_text_for_group(&new_group_id),
                                    ),
                                    Err(error) => {
                                        try_send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            group_id,
                                            &format!(
                                                "{}\nWarning: failed to send the ready message to the new chat: {error:#}",
                                                request.created_text_for_group(&new_group_id)
                                            ),
                                        );
                                    }
                                }
                            }
                            Err(error) => {
                                try_send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &format!("Error: failed to create session: {error:#}"),
                                );
                            }
                        }
                    }
                    RouteAction::ResumeSession(request) => {
                        match resume_parallel_session(
                            config_path,
                            Arc::clone(&wn),
                            subscriptions.clone(),
                            tx.clone(),
                            &request,
                            subscribe_limit,
                        ) {
                            Ok(()) => {
                                if request.group_id == group_id {
                                    try_send_reply_recorded(
                                        &wn,
                                        &event_journal,
                                        group_id,
                                        &request.target_text,
                                    );
                                } else {
                                    match send_reply_recorded(
                                        &wn,
                                        &event_journal,
                                        &request.group_id,
                                        &request.target_text,
                                    ) {
                                        Ok(()) => try_send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            group_id,
                                            &request.reply_text,
                                        ),
                                        Err(error) => {
                                            try_send_reply_recorded(
                                                &wn,
                                                &event_journal,
                                                group_id,
                                                &format!(
                                                    "Error: resumed session locally, but failed to message the target chat: {error:#}"
                                                ),
                                            );
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                try_send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &format!("Error: failed to resume session: {error:#}"),
                                );
                            }
                        }
                    }
                    RouteAction::DownloadMedia(request) => {
                        try_download_wn_media(&app, &wn, &event_journal, group_id, request);
                    }
                    RouteAction::Run(request) => {
                        let mut run_group_id = group_id.to_string();
                        match app.job_session_request(
                            event.group_id.as_deref(),
                            event.sender.as_deref(),
                            &request,
                        ) {
                            Ok(Some(session_request)) => {
                                match create_parallel_session(
                                    config_path,
                                    Arc::clone(&app),
                                    Arc::clone(&wn),
                                    subscriptions.clone(),
                                    tx.clone(),
                                    &session_request,
                                    subscribe_limit,
                                ) {
                                    Ok(new_group_id) => {
                                        try_send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            group_id,
                                            &session_request
                                                .job_started_text_for_group(&new_group_id),
                                        );
                                        let ack = session_request
                                            .job_ready_text(&app.run_ack_text(&request));
                                        match send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            &new_group_id,
                                            &ack,
                                        ) {
                                            Ok(()) => run_group_id = new_group_id,
                                            Err(error) => {
                                                try_send_reply_recorded(
                                                    &wn,
                                                    &event_journal,
                                                    group_id,
                                                    &format!(
                                                        "Warning: created the job session, but failed to send the first message there: {error:#}\nRunning this job here instead."
                                                    ),
                                                );
                                                try_send_reply_recorded(
                                                    &wn,
                                                    &event_journal,
                                                    group_id,
                                                    &app.run_ack_text(&request),
                                                );
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        try_send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            group_id,
                                            &format!(
                                                "Warning: failed to create a job session: {error:#}\nRunning this job here instead."
                                            ),
                                        );
                                        try_send_reply_recorded(
                                            &wn,
                                            &event_journal,
                                            group_id,
                                            &app.run_ack_text(&request),
                                        );
                                    }
                                }
                            }
                            Ok(None) => {
                                try_send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &app.run_ack_text(&request),
                                );
                            }
                            Err(error) => {
                                try_send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &format!(
                                        "Warning: failed to prepare a job session: {error:#}\nRunning this job here instead."
                                    ),
                                );
                                try_send_reply_recorded(
                                    &wn,
                                    &event_journal,
                                    group_id,
                                    &app.run_ack_text(&request),
                                );
                            }
                        }
                        dispatch_agent_request(AgentDispatch {
                            execution,
                            job_queue: job_queue.as_ref(),
                            app: Arc::clone(&app),
                            wn: Arc::clone(&wn),
                            event_journal: Arc::clone(&event_journal),
                            event: &event,
                            source_group_id: group_id,
                            reply_group_id: run_group_id,
                            request,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

struct AgentDispatch<'a> {
    execution: ListenerExecution,
    job_queue: Option<&'a JobQueue>,
    app: Arc<AgentApp>,
    wn: Arc<WnClient>,
    event_journal: Arc<Mutex<EventJournal>>,
    event: &'a MessageEvent,
    source_group_id: &'a str,
    reply_group_id: String,
    request: AgentRequest,
}

fn dispatch_agent_request(dispatch: AgentDispatch<'_>) {
    let mut request = dispatch.request;
    if is_bare_work_chat_followup(
        dispatch.event,
        dispatch.source_group_id,
        &dispatch.app.config().whitenoise.group_id,
    ) {
        match contextualize_bare_followup_request(
            &dispatch.app,
            dispatch.job_queue,
            dispatch.event,
            dispatch.source_group_id,
            request.clone(),
        ) {
            Ok(contextualized) => request = contextualized,
            Err(error) => eprintln!(
                "agentnoise: failed to add work-chat follow-up context for {}: {error:#}",
                dispatch.source_group_id
            ),
        }
    }

    match dispatch.execution {
        ListenerExecution::Inline => {
            run_inline_job(
                dispatch.app,
                dispatch.wn,
                dispatch.event_journal,
                dispatch.reply_group_id,
                request,
            );
        }
        ListenerExecution::Queue => {
            let Some(queue) = dispatch.job_queue else {
                try_send_reply_recorded(
                    &dispatch.wn,
                    &dispatch.event_journal,
                    &dispatch.reply_group_id,
                    "Error: transport queue is not open.",
                );
                return;
            };
            let source_event_id = queue_source_event_id(dispatch.event, dispatch.source_group_id);
            match queue.enqueue(
                &request,
                dispatch.source_group_id,
                &dispatch.reply_group_id,
                &source_event_id,
            ) {
                Ok(outcome) => {
                    if !outcome.inserted {
                        try_send_reply_recorded(
                            &dispatch.wn,
                            &dispatch.event_journal,
                            &dispatch.reply_group_id,
                            &format!("already queued {}", outcome.id),
                        );
                        return;
                    }
                    let worker_running =
                        runtime::role_is_running(dispatch.app.config(), RuntimeRole::Worker)
                            .unwrap_or(false);
                    if !worker_running {
                        try_send_reply_recorded(
                            &dispatch.wn,
                            &dispatch.event_journal,
                            &dispatch.reply_group_id,
                            "queued\nworker: offline\nstart: agentnoise worker start --tmux\nThis now starts a supervised tmux worker that restarts itself if the worker exits.",
                        );
                    }
                }
                Err(error) => {
                    try_send_reply_recorded(
                        &dispatch.wn,
                        &dispatch.event_journal,
                        &dispatch.reply_group_id,
                        &format!("Error: failed to queue job: {error:#}"),
                    );
                }
            }
        }
    }
}

fn is_bare_active_job_followup(
    event: &MessageEvent,
    group_id: &str,
    primary_group_id: &str,
) -> bool {
    is_bare_work_chat_followup(event, group_id, primary_group_id)
}

fn is_bare_work_chat_followup(
    event: &MessageEvent,
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
    event: &MessageEvent,
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

fn contextualize_bare_followup_request(
    app: &AgentApp,
    queue: Option<&JobQueue>,
    event: &MessageEvent,
    group_id: &str,
    mut request: AgentRequest,
) -> Result<AgentRequest> {
    if request.prompt.contains("AgentNoise follow-up context:") {
        return Ok(request);
    }

    let mut context_sections = Vec::new();
    if let Some(queue) = queue
        && let Some(job) = queue.latest_terminal_for_reply_group(group_id)?
    {
        context_sections.push(render_latest_job_context(&job));
    }
    if let Some(session_context) = app.session_context_text_for_group(group_id)? {
        context_sections.push(session_context);
    }

    if context_sections.is_empty() {
        return Ok(request);
    }

    request.prompt = wrap_followup_prompt_with_context(
        &request.prompt,
        event.text.trim(),
        &context_sections.join("\n"),
    );
    Ok(request)
}

fn render_latest_job_context(job: &QueuedJob) -> String {
    let mut lines = vec![format!(
        "latest same-chat job: {} ({})",
        short_ref(&job.id),
        queue_status_label(job.status)
    )];
    lines.push(format!(
        "latest request: {}",
        compact_text(user_request_from_prompt(&job.request.prompt), 320)
    ));
    if let Some(summary) = job
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        lines.push(format!("latest result: {}", compact_text(summary, 900)));
    } else if let Some(error) = job
        .error
        .as_deref()
        .map(str::trim)
        .filter(|error| !error.is_empty())
    {
        lines.push(format!("latest failure: {}", compact_text(error, 900)));
    }
    lines.join("\n")
}

fn wrap_followup_prompt_with_context(prompt: &str, event_text: &str, context: &str) -> String {
    let prompt = prompt.trim();
    let (directive, prompt_body) = split_leading_prompt_directive(prompt);
    let user_request = if event_text.trim().is_empty() {
        prompt_body
    } else {
        event_text
    }
    .trim();

    let mut output = String::new();
    if let Some(directive) = directive {
        output.push_str(directive);
        output.push_str("\n\n");
    }
    output.push_str("AgentNoise follow-up context:\n");
    output.push_str("- The user sent a bare follow-up in this existing White Noise work chat.\n");
    output.push_str(
        "- Resolve words like \"here\", \"this\", \"it\", \"results\", and \"write-up\" against this same chat and latest job unless the user says otherwise.\n",
    );
    output.push_str(context.trim());
    output.push_str("\n\nUser request:\n");
    output.push_str(user_request);
    output
}

fn prompt_without_leading_directive(prompt: &str) -> &str {
    split_leading_prompt_directive(prompt).1
}

fn user_request_from_prompt(prompt: &str) -> &str {
    let prompt = prompt_without_leading_directive(prompt);
    if let Some(index) = prompt.rfind("User request:") {
        return prompt[index + "User request:".len()..].trim();
    }
    prompt
}

fn split_leading_prompt_directive(prompt: &str) -> (Option<&'static str>, &str) {
    let prompt = prompt.trim();
    for directive in ["@wiki", "wiki"] {
        if prompt == directive {
            return (Some(directive), "");
        }
        if let Some(rest) = prompt.strip_prefix(directive)
            && rest.chars().next().is_some_and(char::is_whitespace)
        {
            return (Some(directive), rest.trim());
        }
    }
    (None, prompt)
}

fn queue_status_label(status: QueueStatus) -> &'static str {
    match status {
        QueueStatus::Queued => "queued",
        QueueStatus::Claimed => "claimed",
        QueueStatus::Running => "running",
        QueueStatus::Succeeded => "succeeded",
        QueueStatus::Failed => "failed",
    }
}

fn run_inline_job(
    app: Arc<AgentApp>,
    wn: Arc<WnClient>,
    event_journal: Arc<Mutex<EventJournal>>,
    group_id: String,
    request: AgentRequest,
) {
    std::thread::spawn(move || {
        let request_for_media = request.clone();
        let progress_wn = Arc::clone(&wn);
        let progress_journal = Arc::clone(&event_journal);
        let progress_group = group_id.clone();
        let result = app.run_request_record_with_progress(request, move |text| {
            if let Err(error) =
                send_reply_recorded(&progress_wn, &progress_journal, &progress_group, &text)
            {
                eprintln!("agentnoise: failed to send progress reply: {error:#}");
            }
        });
        let reply = match &result {
            Ok(record) => app.render_job_record(record),
            Err(error) => {
                format!("Error: job failed to start: {error:#}")
            }
        };
        if let Err(error) = send_reply_recorded(&wn, &event_journal, &group_id, &reply) {
            eprintln!("agentnoise: failed to send job reply: {error:#}");
        }
        if let Ok(record) = result {
            upload_referenced_job_media(
                &app,
                &wn,
                &event_journal,
                &group_id,
                &request_for_media,
                &record,
            );
        }
    });
}

fn queue_source_event_id(event: &MessageEvent, group_id: &str) -> String {
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
            "Attached White Noise media ingested by agentnoise:\n{}\nUse these local file paths when inspecting the attached media.",
            self.prompt_lines.join("\n")
        ))
    }

    fn wiki_ingest_context(&self) -> Option<String> {
        if self.prompt_lines.is_empty() {
            return None;
        }
        Some(format!(
            "Attached White Noise media for the LLM Wiki ingest pipeline:\n{}\nUse the wiki File Ingestion workflow for media/files: create immutable raw metadata stubs for these local file sources, include file paths plus visible-content or file-metadata descriptions/provenance, then continue with the user's wiki request.",
            self.prompt_lines.join("\n")
        ))
    }
}

fn ingest_wn_attachments(
    app: &AgentApp,
    wn: &WnClient,
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

        let Some(hash) = attachment
            .hash
            .as_deref()
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
        else {
            result.reply_lines.push(format!(
                "{media_label} saved but not downloaded: {display_name} has no White Noise media hash"
            ));
            result.prompt_lines.push(format!(
                "- {media_label} {display_name}: metadata saved as {}, but no downloadable media hash was present",
                record.id
            ));
            continue;
        };

        match wn.download_media_from(group_id, hash) {
            Ok(media) => {
                let Some(source_path) = media.file_path.as_deref() else {
                    result.reply_lines.push(format!(
                        "{media_label} saved but not downloaded: White Noise did not return a local path for {display_name}"
                    ));
                    result.prompt_lines.push(format!(
                        "- {media_label} {display_name}: metadata saved as {}, but White Noise did not return a local path",
                        record.id
                    ));
                    continue;
                };
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

fn upload_referenced_job_media(
    app: &AgentApp,
    wn: &WnClient,
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
            .unwrap_or("image");
        let caption = format!("agent output: {file_name}");
        if let Err(error) = wn.upload_media_to(group_id, &path, Some(&caption)) {
            eprintln!(
                "agentnoise: failed to upload referenced media {}: {error:#}",
                path.display()
            );
            try_send_reply_recorded(
                wn,
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
    let mut seen = HashSet::new();
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

fn try_download_wn_media(
    app: &AgentApp,
    wn: &WnClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    request: agentnoise::app::MediaDownloadAction,
) {
    let reply = match wn.download_media_from(group_id, &request.original_file_hash) {
        Ok(media) => {
            let Some(path) = media.file_path.as_deref() else {
                return try_send_reply_recorded(
                    wn,
                    event_journal,
                    group_id,
                    "downloaded\nWhite Noise did not return a local file path.",
                );
            };
            let size = match copy_private_file(path, &request.output_path) {
                Ok(size) => size,
                Err(error) => {
                    return try_send_reply_recorded(
                        wn,
                        event_journal,
                        group_id,
                        &format!("Error: saving download failed: {error:#}"),
                    );
                }
            };
            if let Err(error) = app.record_attachment_downloaded(
                &request.record_id,
                request.attachment_index,
                &request.output_path,
                Some(size),
            ) {
                eprintln!("agentnoise: failed to update attachment store: {error:#}");
            }
            let mut lines = vec![format!("downloaded {}", request.output_path.display())];
            lines.push(format!("{size} bytes"));
            if path != request.output_path {
                lines.push(format!("source: {}", path.display()));
            }
            lines.join("\n")
        }
        Err(error) => format!("Error: download failed: {error:#}"),
    };
    try_send_reply_recorded(wn, event_journal, group_id, &reply);
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

fn send_startup_hello_if_needed(
    config: &Config,
    wn: &WnClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    sent: &mut HashSet<String>,
    group_id: &str,
) {
    if !should_send_startup_hello(config, sent, group_id) {
        return;
    }
    let text = startup_hello_text(config);
    if let Err(error) = send_reply_recorded(wn, event_journal, group_id, &text) {
        eprintln!("agentnoise: failed to send startup hello to {group_id}: {error:#}");
    }
}

fn should_send_startup_hello(config: &Config, sent: &mut HashSet<String>, group_id: &str) -> bool {
    let group_id = group_id.trim();
    let primary_group = config.whitenoise.group_id.trim();
    !group_id.is_empty()
        && !primary_group.is_empty()
        && group_id == primary_group
        && !config.whitenoise.allowed_senders.is_empty()
        && sent.insert(group_id.to_string())
}

fn startup_hello_text(config: &Config) -> String {
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    render_startup_hello(config, &timestamp)
}

fn render_startup_hello(config: &Config, timestamp: &str) -> String {
    let profile = config.whitenoise.profile_display_name.trim();
    let workspace = config
        .default_repo_alias()
        .map(|alias| format!("{alias}:/"))
        .unwrap_or_else(|| "none".to_string());
    let mut lines = vec![format!("agentnoise up {}", compact_timestamp(timestamp))];
    if !profile.is_empty() {
        lines.push(profile.to_string());
    }
    lines.push(workspace);
    lines.push("main: /codex /claude /wiki opens work chats".to_string());
    lines.push("work chats: just talk".to_string());
    lines.push("/status /doctor /help".to_string());
    lines.join("\n")
}

fn send_reply_recorded(
    wn: &WnClient,
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

fn try_send_reply_recorded(
    wn: &WnClient,
    event_journal: &Arc<Mutex<EventJournal>>,
    group_id: &str,
    text: &str,
) {
    if let Err(error) = send_reply_recorded(wn, event_journal, group_id, text) {
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
    subscriptions: SubscriptionStateHandle,
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
    subscribe_group_if_needed(wn, subscriptions, tx, &group_id, subscribe_limit)?;

    Ok(group_id)
}

fn resume_parallel_session(
    config_path: &Path,
    wn: Arc<WnClient>,
    subscriptions: SubscriptionStateHandle,
    tx: mpsc::Sender<StreamItem>,
    request: &agentnoise::app::ResumeSessionRequest,
    subscribe_limit: u32,
) -> Result<()> {
    persist_control_group_id(config_path, &request.group_id)?;
    subscribe_group_if_needed(wn, subscriptions, tx, &request.group_id, subscribe_limit)
}

fn persist_control_group_id(config_path: &Path, group_id: &str) -> Result<()> {
    let mut config = Config::load(config_path)?;
    config.whitenoise.add_control_group_id(group_id);
    config.save(config_path)
}

fn initial_group_ids(wn: &WnClient) -> Vec<String> {
    match wn.discover_group_ids() {
        Ok(discovered) => merge_initial_group_ids(wn.configured_group_ids(), discovered),
        Err(error) => {
            eprintln!("agentnoise: group discovery failed: {error:#}");
            wn.configured_group_ids()
        }
    }
}

fn merge_initial_group_ids(configured: Vec<String>, discovered: Vec<String>) -> Vec<String> {
    let discovered = unique_group_ids(discovered);
    if discovered.is_empty() {
        return unique_group_ids(configured);
    }
    discovered
}

fn subscribe_group_if_needed(
    wn: Arc<WnClient>,
    subscriptions: SubscriptionStateHandle,
    tx: mpsc::Sender<StreamItem>,
    group_id: &str,
    subscribe_limit: u32,
) -> Result<()> {
    let group_id = group_id.trim();
    if group_id.is_empty() {
        return Ok(());
    }

    {
        let subscriptions = subscriptions
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription registry lock poisoned"))?;
        if subscriptions.is_running(group_id) {
            return Ok(());
        }
    }

    let group_id = group_id.to_string();
    {
        let mut registry = subscriptions
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription registry lock poisoned"))?;
        registry.mark_starting(&group_id);
        save_subscriptions_snapshot(&subscriptions, &registry);
    }
    let mut child = wn
        .subscribe_group_with_limit(&group_id, subscribe_limit)
        .with_context(|| format!("starting White Noise subscription for {group_id}"))?;
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .context("wn subscribe did not expose stdout")?;
    {
        let mut registry = subscriptions
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription registry lock poisoned"))?;
        registry.mark_started(&group_id, pid);
        save_subscriptions_snapshot(&subscriptions, &registry);
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
                Ok(value) => {
                    let _ = tx.send(StreamItem::StreamJson {
                        group_id: group_id.clone(),
                    });
                    value
                }
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
                if tx
                    .send(StreamItem::Event {
                        event,
                        source: MessageSource::Stream,
                    })
                    .is_err()
                {
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

fn spawn_subscription_watchdog(
    subscriptions: Arc<Mutex<SubscriptionRegistry>>,
    tx: mpsc::Sender<StreamItem>,
) {
    thread::spawn(move || {
        loop {
            thread::sleep(SUBSCRIPTION_RECONCILE_INTERVAL);
            let groups = subscriptions
                .lock()
                .map(|registry| registry.due_for_reconciliation(SUBSCRIPTION_RECONCILE_INTERVAL))
                .unwrap_or_default();
            for group_id in groups {
                if tx.send(StreamItem::Reconcile { group_id }).is_err() {
                    return;
                }
            }
        }
    });
}

fn spawn_group_reconciliation(wn: Arc<WnClient>, tx: mpsc::Sender<StreamItem>, group_id: String) {
    thread::spawn(
        move || match wn.list_group_messages(&group_id, SUBSCRIPTION_RECONCILE_LIMIT) {
            Ok(messages) => {
                let _ = tx.send(StreamItem::PolledMessages { group_id, messages });
            }
            Err(error) => {
                let _ = tx.send(StreamItem::ReconcileError {
                    group_id,
                    message: format!("{error:#}"),
                });
            }
        },
    );
}

fn schedule_subscription_restart(
    tx: mpsc::Sender<StreamItem>,
    group_id: String,
    restart_count: u32,
) {
    thread::spawn(move || {
        let delay = subscription_restart_delay(restart_count);
        thread::sleep(delay);
        let _ = tx.send(StreamItem::RestartSubscription { group_id });
    });
}

fn subscription_restart_delay(restart_count: u32) -> Duration {
    let seconds = 2u64.saturating_pow(restart_count.min(5));
    Duration::from_secs(seconds.min(30))
}

#[cfg(unix)]
fn terminate_subscription_process(pid: u32) {
    match std::process::Command::new("kill")
        .arg(pid.to_string())
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("agentnoise: stopping stale wn subscription {pid} exited with {status}");
        }
        Err(error) => {
            eprintln!("agentnoise: failed to stop stale wn subscription {pid}: {error:#}");
        }
    }
}

#[cfg(not(unix))]
fn terminate_subscription_process(_pid: u32) {}

fn save_subscriptions_snapshot(
    subscriptions: &SubscriptionStateHandle,
    registry: &SubscriptionRegistry,
) {
    if let Err(error) =
        subscriptions::write_snapshot(&subscriptions.snapshot_path, &registry.snapshot())
    {
        eprintln!(
            "agentnoise: failed to write subscription snapshot {}: {error:#}",
            subscriptions.snapshot_path.display()
        );
    }
}

fn reconciled_events_after(
    messages: &[MessageEvent],
    previous_id: Option<&str>,
) -> Vec<MessageEvent> {
    let Some(previous_id) = previous_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    match messages
        .iter()
        .position(|event| event.id.as_deref().map(str::trim) == Some(previous_id))
    {
        Some(index) => messages[index + 1..].to_vec(),
        None => messages.to_vec(),
    }
}

fn spawn_local_session_watcher(config: &Config, tx: mpsc::Sender<StreamItem>) {
    if !config.local_sessions.watch {
        return;
    }

    let interval = Duration::from_secs(config.local_sessions.watch_interval_seconds.max(1));
    let notify_limit = config.local_sessions.notify_limit.clamp(1, 20);
    println!(
        "agentnoise local session watcher enabled; notifying up to {notify_limit} new sessions"
    );

    thread::spawn(move || {
        let mut seen: Option<HashSet<String>> = None;
        let mut last_error: Option<String> = None;

        loop {
            match local_sessions::discover_all_local_sessions() {
                Ok(sessions) => {
                    last_error = None;
                    match &mut seen {
                        Some(seen_keys) => {
                            let mut new_sessions = sessions
                                .into_iter()
                                .filter(|session| {
                                    seen_keys.insert(local_sessions::local_session_key(session))
                                })
                                .collect::<Vec<_>>();
                            if !new_sessions.is_empty() {
                                new_sessions.truncate(notify_limit);
                                if tx
                                    .send(StreamItem::LocalSessionsChanged(new_sessions))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        None => {
                            seen = Some(
                                sessions
                                    .iter()
                                    .map(local_sessions::local_session_key)
                                    .collect(),
                            );
                        }
                    }
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    if last_error.as_deref() != Some(message.as_str()) {
                        if tx
                            .send(StreamItem::LocalSessionsWatchError(message.clone()))
                            .is_err()
                        {
                            break;
                        }
                        last_error = Some(message);
                    }
                }
            }

            thread::sleep(interval);
        }
    });
}

fn local_session_notification_group(
    config_path: &Path,
    startup_config: &Config,
) -> Result<Option<String>> {
    let config = if config_path.exists() {
        Config::load(config_path)
            .with_context(|| format!("loading current config {}", config_path.display()))?
    } else {
        startup_config.clone()
    };
    Ok(local_session_notification_group_from_config(&config))
}

fn local_session_notification_group_from_config(config: &Config) -> Option<String> {
    if !config.local_sessions.watch || config.whitenoise.allowed_senders.is_empty() {
        return None;
    }
    let group_id = config.whitenoise.group_id.trim();
    (!group_id.is_empty()).then(|| group_id.to_string())
}

fn extend_unique(group_ids: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for group_id in more {
        let group_id = group_id.trim();
        if !group_id.is_empty() && !group_ids.iter().any(|existing| existing == group_id) {
            group_ids.push(group_id.to_string());
        }
    }
}

fn unique_group_ids(group_ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut output = Vec::new();
    extend_unique(&mut output, group_ids);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_event(group_id: &str, id: &str, text: &str) -> MessageEvent {
        MessageEvent {
            group_id: Some(group_id.to_string()),
            sender: Some("phone".to_string()),
            text: text.to_string(),
            unsupported: None,
            id: Some(id.to_string()),
            trigger: None,
            is_initial: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn startup_hello_includes_time_profile_and_workspace() {
        let mut config = Config::template();
        config.whitenoise.profile_display_name = "m5".to_string();

        let text = render_startup_hello(&config, "2026-05-15T20:00:00Z");

        assert_eq!(
            text,
            "agentnoise up 20:00Z\n\
             m5\n\
             sandbox:/\n\
             main: /codex /claude /wiki opens work chats\n\
             work chats: just talk\n\
             /status /doctor /help"
        );
    }

    #[test]
    fn startup_hello_requires_pairing_and_only_targets_primary_group() {
        let mut config = Config::template();
        config.whitenoise.group_id = "group-a".to_string();
        let mut sent = HashSet::new();

        assert!(!should_send_startup_hello(&config, &mut sent, "group-a"));

        config
            .whitenoise
            .allowed_senders
            .push("npub1pairedphone".to_string());

        assert!(should_send_startup_hello(&config, &mut sent, "group-a"));
        assert!(!should_send_startup_hello(&config, &mut sent, "group-a"));
        assert!(!should_send_startup_hello(&config, &mut sent, "group-b"));
        assert!(!should_send_startup_hello(&config, &mut sent, " "));
    }

    #[test]
    fn pending_proposal_send_retries_back_off_more() {
        assert!(
            send_retry_delay(
                "MDK error: Can't create message because a pending proposal exists.",
                2
            ) > send_retry_delay("temporary transport failure", 2)
        );
    }

    #[test]
    fn launcher_flags_default_direct_or_bondage_override() {
        assert_eq!(launcher_from_flags(false, false).unwrap(), None);
        assert_eq!(
            launcher_from_flags(true, false).unwrap(),
            Some(RunnerLauncher::Direct)
        );
        assert_eq!(
            launcher_from_flags(false, true).unwrap(),
            Some(RunnerLauncher::Bondage)
        );
    }

    #[test]
    fn launcher_flags_reject_conflicting_modes() {
        let error = launcher_from_flags(true, true).unwrap_err().to_string();
        assert!(error.contains("--direct-agents and --bondage cannot be combined"));
    }

    #[test]
    fn worker_tmux_supervisor_restarts_worker_process() {
        let command = supervised_worker_shell_command(
            Path::new("/opt/homebrew/bin/agentnoise"),
            Path::new("/Users/me/Library/Application Support/agentnoise/config.toml"),
            3,
        );

        assert!(command.starts_with("while true; do "));
        assert!(!command.contains("' /opt/homebrew/bin/agentnoise'"));
        assert!(command.contains("'/opt/homebrew/bin/agentnoise'"));
        assert!(
            command.contains(
                "--config '/Users/me/Library/Application Support/agentnoise/config.toml'"
            )
        );
        assert!(command.contains("worker start --poll-seconds 3"));
        assert!(command.contains("restarting in 5s"));
    }

    #[test]
    fn worker_tmux_session_name_is_instance_scoped() {
        assert_eq!(
            worker_tmux_session_name(&Config::template()),
            "agentnoise-worker"
        );
        assert_eq!(
            worker_tmux_session_name(&Config::template_for_instance("darkmatter")),
            "agentnoise-worker-darkmatter"
        );
    }

    #[test]
    fn contextualized_prompt_latest_request_strips_prior_context() {
        let prompt = "@wiki\n\nAgentNoise follow-up context:\nlatest result: previous\n\nUser request:\nShow me the write up here";

        assert_eq!(
            user_request_from_prompt(prompt),
            "Show me the write up here"
        );
    }

    #[test]
    fn subscription_reconciliation_baselines_before_replaying_messages() {
        let messages = vec![
            message_event("group-a", "m1", "/status"),
            message_event("group-a", "m2", "/jobs"),
        ];

        assert!(reconciled_events_after(&messages, None).is_empty());
        assert_eq!(
            reconciled_events_after(&messages, Some("m1"))
                .into_iter()
                .map(|event| event.id.unwrap())
                .collect::<Vec<_>>(),
            vec!["m2".to_string()]
        );
        assert_eq!(reconciled_events_after(&messages, Some("missing")).len(), 2);
    }

    #[test]
    fn bare_text_in_active_work_chat_is_treated_as_followup() {
        let event = message_event("worker", "m1", "Give me list");

        assert!(is_bare_active_job_followup(&event, "worker", "inbox"));
        assert!(!is_bare_active_job_followup(&event, "inbox", "inbox"));

        let slash = message_event("worker", "m2", "/tail an-123");
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
            source_event_id: "m1".to_string(),
            created_at: "2026-06-02T18:00:00Z".to_string(),
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
    fn bare_work_chat_followup_prompt_includes_latest_same_chat_job_context() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.group_id = "inbox".to_string();
        config.whitenoise.group_ids = vec!["inbox".to_string(), "worker".to_string()];
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config.clone(), None).unwrap();

        let mut state = agentnoise::session::SessionState::new(Some("work".to_string()));
        state.name = Some("frontier - old topic label".to_string());
        state.default_agent = Some(AgentKind::Codex);
        state.default_prompt_prefix = Some("@wiki".to_string());
        app.create_session_record("worker", state).unwrap();

        let queue = JobQueue::open(config.resolved_queue_path()).unwrap();
        let previous_request = AgentRequest::new(AgentKind::Codex, "work", "@wiki Try again");
        let previous = queue
            .enqueue(&previous_request, "worker", "worker", "event-1")
            .unwrap();
        queue.mark_running(&previous.id).unwrap();
        queue
            .mark_succeeded(
                &previous.id,
                "Saved the speed-test screenshot in home-networking and updated the dual-Starlink write-up.",
                None,
            )
            .unwrap();

        let event = message_event("worker", "event-2", "Show me the write up here");
        let request = match app
            .route_message(
                event.group_id.as_deref(),
                event.sender.as_deref(),
                &event.text,
            )
            .unwrap()
        {
            RouteAction::Run(request) => request,
            other => panic!("expected run action, got {other:?}"),
        };
        let contextualized =
            contextualize_bare_followup_request(&app, Some(&queue), &event, "worker", request)
                .unwrap();

        assert!(
            contextualized
                .prompt
                .starts_with("@wiki\n\nAgentNoise follow-up context:")
        );
        assert!(contextualized.prompt.contains("latest same-chat job: q-"));
        assert!(contextualized.prompt.contains("latest request: Try again"));
        assert!(
            contextualized
                .prompt
                .contains("latest result: Saved the speed-test screenshot")
        );
        assert!(
            contextualized
                .prompt
                .contains("User request:\nShow me the write up here")
        );
    }

    #[test]
    fn subscription_restart_delay_backs_off_and_caps() {
        assert_eq!(subscription_restart_delay(0), Duration::from_secs(1));
        assert_eq!(subscription_restart_delay(1), Duration::from_secs(2));
        assert_eq!(subscription_restart_delay(5), Duration::from_secs(30));
        assert_eq!(subscription_restart_delay(30), Duration::from_secs(30));
    }

    #[test]
    fn local_session_notifications_are_opt_in_and_primary_chat_only() {
        let mut config = Config::template();
        config.whitenoise.group_id = "group-a".to_string();
        config
            .whitenoise
            .allowed_senders
            .push("npub1pairedphone".to_string());

        assert_eq!(local_session_notification_group_from_config(&config), None);

        config.local_sessions.watch = true;
        assert_eq!(
            local_session_notification_group_from_config(&config),
            Some("group-a".to_string())
        );

        config.whitenoise.group_id.clear();
        assert_eq!(local_session_notification_group_from_config(&config), None);
    }

    #[test]
    fn local_session_notifications_use_reloaded_pairing_state() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut startup_config = Config::template();
        startup_config.local_sessions.watch = true;
        startup_config.whitenoise.group_id = "group-a".to_string();
        startup_config.whitenoise.allowed_senders.clear();
        startup_config.save(&config_path).unwrap();

        assert_eq!(
            local_session_notification_group(&config_path, &startup_config).unwrap(),
            None
        );

        let mut paired_config = startup_config.clone();
        paired_config
            .whitenoise
            .allowed_senders
            .push("npub1pairedphone".to_string());
        paired_config.save(&config_path).unwrap();

        assert_eq!(
            local_session_notification_group(&config_path, &startup_config).unwrap(),
            Some("group-a".to_string())
        );
    }

    #[test]
    fn initial_group_merge_uses_discovered_groups_when_available() {
        let groups = merge_initial_group_ids(
            vec!["stale".to_string(), "active".to_string()],
            vec!["active".to_string()],
        );

        assert_eq!(groups, vec!["active".to_string()]);
    }

    #[test]
    fn attachment_context_is_added_to_agent_runs() {
        let mut ingest = AttachmentIngestResult::new("att-123".to_string(), "1 file".to_string());
        ingest
            .prompt_lines
            .push("- shot.png: /tmp/agentnoise/shot.png (42 bytes)".to_string());

        let action = action_with_attachment_context(
            RouteAction::Run(AgentRequest::prompt(AgentKind::Codex, "inspect this")),
            &ingest,
        );

        assert!(matches!(
            action,
            RouteAction::Run(request)
                if request.prompt.contains("inspect this")
                    && request.prompt.contains("Attached White Noise media ingested")
                    && request.prompt.contains("/tmp/agentnoise/shot.png")
        ));
    }

    #[test]
    fn wiki_attachment_context_uses_ingest_pipeline_language() {
        let mut ingest = AttachmentIngestResult::new("att-123".to_string(), "1 file".to_string());
        ingest.prompt_lines.push(
            "- shot.png: /workspace/.agentnoise/attachments/att-123/01-shot.png (42 bytes)"
                .to_string(),
        );

        let action = action_with_attachment_context(
            RouteAction::Run(AgentRequest::prompt(AgentKind::Codex, "@wiki catalog this")),
            &ingest,
        );

        assert!(matches!(
            action,
            RouteAction::Run(request)
                if request.prompt.contains("LLM Wiki ingest pipeline")
                    && request.prompt.contains("raw metadata stubs")
                    && request.prompt.contains(".agentnoise/attachments")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn ingest_wn_attachments_downloads_supported_media_and_records_local_path() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.pdf");
        std::fs::write(&source, "%PDF bytes").unwrap();
        let wn_bin = temp.path().join("wn");
        std::fs::write(
            &wn_bin,
            format!(
                r#"#!/bin/sh
printf '%s\n' '{{"result":{{"file_path":"{}","original_file_hash":"{}"}}}}'
"#,
                source.display(),
                "11".repeat(32)
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&wn_bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wn_bin, permissions).unwrap();

        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.whitenoise.wn_bin = wn_bin.display().to_string();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config.clone(), None).unwrap();
        let wn = WnClient::new(config.whitenoise);
        let event = MessageEvent {
            group_id: Some("group-a".to_string()),
            sender: Some("phone".to_string()),
            text: String::new(),
            unsupported: None,
            id: Some("msg1".to_string()),
            trigger: None,
            is_initial: false,
            attachments: vec![attachments::AttachmentInfo {
                kind: "media_attachments".to_string(),
                name: Some("report.pdf".to_string()),
                mime_type: Some("application/pdf".to_string()),
                url: None,
                size: None,
                hash: Some("11".repeat(32)),
                local_path: None,
            }],
        };
        let action = match app.route_unsupported_event(&event).unwrap() {
            RouteAction::IngestAttachments(action) => action,
            other => panic!("expected ingest action, got {other:?}"),
        };

        let ingest = ingest_wn_attachments(&app, &wn, "group-a", Some("phone"), action);

        assert!(
            ingest
                .reply_text()
                .contains("media ingested: PDF report.pdf")
        );
        let details = match app
            .route_message(Some("group-a"), Some("phone"), "/attach 1")
            .unwrap()
        {
            RouteAction::Reply(reply) => reply,
            other => panic!("expected reply, got {other:?}"),
        };
        assert!(details.contains("local:"));
        assert!(details.contains("01-report.pdf"));
        assert!(repo.path().join(".agentnoise/attachments").exists());
    }

    #[cfg(unix)]
    #[test]
    fn ingest_wn_attachments_copies_existing_local_media_into_workspace() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let source = temp.path().join("wn-media-cache.png");
        std::fs::write(&source, "png bytes from wn cache").unwrap();

        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config.clone(), None).unwrap();
        let wn = WnClient::new(config.whitenoise);
        let event = MessageEvent {
            group_id: Some("group-a".to_string()),
            sender: Some("phone".to_string()),
            text: String::new(),
            unsupported: None,
            id: Some("msg1".to_string()),
            trigger: None,
            is_initial: false,
            attachments: vec![attachments::AttachmentInfo {
                kind: "media_attachments".to_string(),
                name: Some("shot.png".to_string()),
                mime_type: Some("image/png".to_string()),
                url: None,
                size: None,
                hash: None,
                local_path: Some(source.display().to_string()),
            }],
        };
        let action = match app.route_unsupported_event(&event).unwrap() {
            RouteAction::IngestAttachments(action) => action,
            other => panic!("expected ingest action, got {other:?}"),
        };

        let ingest = ingest_wn_attachments(&app, &wn, "group-a", Some("phone"), action);

        assert!(
            ingest
                .reply_text()
                .contains("media ingested: image shot.png")
        );
        assert!(
            ingest
                .prompt_context()
                .unwrap()
                .contains("Attached White Noise media")
        );
        assert!(
            ingest
                .prompt_context()
                .unwrap()
                .contains(".agentnoise/attachments")
        );
        let copied = repo
            .path()
            .join(".agentnoise/attachments")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("01-shot.png");
        assert_eq!(std::fs::read(&copied).unwrap(), b"png bytes from wn cache");
        assert_eq!(
            std::fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let details = match app
            .route_message(Some("group-a"), Some("phone"), "/attach 1")
            .unwrap()
        {
            RouteAction::Reply(reply) => reply,
            other => panic!("expected reply, got {other:?}"),
        };
        assert!(details.contains(&copied.display().to_string()));
        assert!(!details.contains(&source.display().to_string()));
    }

    #[test]
    fn referenced_media_paths_are_workspace_scoped() {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let image = repo.path().join("chart.png");
        let pdf = repo.path().join("report.pdf");
        let outside_video = outside.path().join("secret.mp4");
        let unsupported = repo.path().join("notes.txt");
        let input_image = repo
            .path()
            .join(".agentnoise/attachments/att-123/01-shot.png");
        std::fs::write(&image, "png").unwrap();
        std::fs::write(&pdf, "pdf").unwrap();
        std::fs::write(&outside_video, "mp4").unwrap();
        std::fs::write(&unsupported, "txt").unwrap();
        std::fs::create_dir_all(input_image.parent().unwrap()).unwrap();
        std::fs::write(&input_image, "png").unwrap();

        let mut config = Config::template();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        let request = AgentRequest::new(AgentKind::Codex, "work", "make chart");
        let text = format!(
            "Wrote ![chart](chart.png), report.pdf, notes.txt, read {}, also see {}.",
            input_image.display(),
            outside_video.display(),
        );

        let paths = referenced_media_paths(&text, &config, &request);

        assert_eq!(
            paths,
            vec![image.canonicalize().unwrap(), pdf.canonicalize().unwrap()]
        );
    }

    #[test]
    fn initial_group_merge_falls_back_to_configured_groups_without_discovery() {
        let groups = merge_initial_group_ids(
            vec![
                "configured".to_string(),
                "configured".to_string(),
                " ".to_string(),
            ],
            Vec::new(),
        );

        assert_eq!(groups, vec!["configured".to_string()]);
    }
}
