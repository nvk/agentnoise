use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use agentnoise::app::{AgentApp, NewSessionRequest, RouteAction};
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
use agentnoise::runner::{AgentKind, AgentRequest};
use agentnoise::runtime::{self, AcquireMode, EngineGuard, RuntimePairingInfo, RuntimePairingPin};
use agentnoise::service::{self, ServiceTarget};
use agentnoise::setup::{self, SetupOptions, SetupResult};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

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

    publish_identity_profile(config)?;
    println!("profile: saved and published");
    Ok(())
}

fn publish_identity_profile(config: &Config) -> Result<()> {
    let Some(account_ref) = config
        .darkmatter
        .account
        .as_deref()
        .or(config.darkmatter.bot_npub.as_deref())
        .map(str::trim)
        .filter(|account| !account.is_empty())
    else {
        bail!(
            "profile saved, but no desktop identity is configured yet; run `agentnoise setup` or `agentnoise listen` once"
        );
    };

    let bootstrap_relays = config.darkmatter.message_relays.clone();
    if bootstrap_relays.is_empty() {
        bail!("profile saved, but darkmatter.message_relays is empty; cannot publish profile");
    }

    let dm_home = config.resolved_data_dir().join("darkmatter");
    let keychain_service =
        agentnoise::darkmatter_app::keychain_service_for_instance(config.instance.as_deref());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for profile publish")?;

    runtime.block_on(async {
        let engine = DarkmatterEngine::open(
            dm_home,
            bootstrap_relays,
            &keychain_service,
            config.darkmatter.dev_burner_nsec,
        )?;
        engine.start().await?;
        let result = async {
            let Some(account) = engine.find_account(account_ref)? else {
                bail!(
                    "profile saved, but configured darkmatter account was not found in the local keychain/home"
                );
            };
            engine
                .publish_configured_profile(&account.account_id_hex, &config.darkmatter)
                .await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        engine.shutdown().await;
        result
    })
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
    runtime::engine_is_running(&config)
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

fn start_listener(config_path: &Path, args: StartArgs, mode: ListenerMode) -> Result<()> {
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
    run_listener(config_path, config, pairing, guard)
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
        pin_seconds: config.darkmatter.pairing_pin_seconds,
        current_pin: None,
    })
}

fn run_listener(
    config_path: &Path,
    config: Config,
    pairing: Option<PairingRuntime>,
    _guard: EngineGuard,
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
    match tokio_handle.block_on(tokio::time::timeout(
        Duration::from_secs(30),
        engine.publish_discovery(&account_id_hex, &config.darkmatter),
    )) {
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

    let max_chars = config.darkmatter.max_message_chars;
    let dm = Arc::new(DmClient::new(
        engine.clone(),
        account_id_hex.clone(),
        max_chars,
        tokio_handle.clone(),
    ));

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
    let subscribed_groups = Arc::new(Mutex::new(
        group_ids.iter().cloned().collect::<HashSet<_>>(),
    ));
    for group_id in &group_ids {
        spawn_group_subscription(&tokio_handle, Arc::clone(&dm), group_id.clone(), tx.clone());
    }
    spawn_local_session_watcher_simple(config_path, &config, &dm, &event_journal);
    let subscription_tx = tx.clone();
    spawn_group_join_discovery(
        &tokio_handle,
        engine.clone(),
        Arc::clone(&dm),
        account_id_hex.clone(),
        config_path.to_path_buf(),
        Arc::clone(&subscribed_groups),
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

        let action = match app.route_message(
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

        match action {
            RouteAction::Ignore => {}
            RouteAction::Reply(reply) => {
                if let Err(error) = dm.send_reply_to_blocking(&group_id, &reply) {
                    eprintln!("agentnoise: failed to send reply: {error:#}");
                }
            }
            RouteAction::NewSession(request) => {
                match activate_darkmatter_session_group(
                    &SessionGroupContext {
                        config_path,
                        app: app.as_ref(),
                        engine: &engine,
                        account_id_hex: &account_id_hex,
                        subscription: GroupSubscriptionContext {
                            tokio_handle: &tokio_handle,
                            dm: &dm,
                            tx: &subscription_tx,
                            subscribed_groups: &subscribed_groups,
                        },
                    },
                    &request,
                ) {
                    Ok(new_group_id) => {
                        if let Err(error) = dm.send_reply_to_blocking(
                            &group_id,
                            &request.created_text_for_group(&new_group_id),
                        ) {
                            eprintln!("agentnoise: failed to send new-session ack: {error:#}");
                        }
                        if let Err(error) =
                            dm.send_reply_to_blocking(&new_group_id, &request.ready_text())
                        {
                            eprintln!(
                                "agentnoise: failed to send new-session ready message: {error:#}"
                            );
                        }
                    }
                    Err(error) => {
                        let _ = dm.send_reply_to_blocking(
                            &group_id,
                            &format!("Error: failed to create Darkmatter session chat: {error:#}"),
                        );
                    }
                }
            }
            RouteAction::ResumeSession(request) => {
                if let Err(error) = dm.send_reply_to_blocking(&group_id, &request.reply_text) {
                    eprintln!("agentnoise: failed to send resume ack: {error:#}");
                }
                if let Err(error) =
                    dm.send_reply_to_blocking(&request.group_id, &request.target_text)
                {
                    eprintln!("agentnoise: failed to send resume target message: {error:#}");
                    let _ = dm.send_reply_to_blocking(
                        &group_id,
                        &format!(
                            "agentnoise: failed to send resume message to target chat: {error:#}"
                        ),
                    );
                }
            }
            RouteAction::Run(request) => {
                let mut group = group_id.clone();
                match app.job_session_request(
                    event.group_id.as_deref(),
                    event.sender.as_deref(),
                    &request,
                ) {
                    Ok(Some(session_request)) => {
                        match activate_darkmatter_session_group(
                            &SessionGroupContext {
                                config_path,
                                app: app.as_ref(),
                                engine: &engine,
                                account_id_hex: &account_id_hex,
                                subscription: GroupSubscriptionContext {
                                    tokio_handle: &tokio_handle,
                                    dm: &dm,
                                    tx: &subscription_tx,
                                    subscribed_groups: &subscribed_groups,
                                },
                            },
                            &session_request,
                        ) {
                            Ok(new_group_id) => {
                                let ack = app.run_ack_text(&request);
                                if let Err(error) = dm.send_reply_to_blocking(
                                    &group_id,
                                    &session_request.job_started_text_for_group(&new_group_id),
                                ) {
                                    eprintln!(
                                        "agentnoise: failed to send job session ack: {error:#}"
                                    );
                                }
                                if let Err(error) = dm.send_reply_to_blocking(
                                    &new_group_id,
                                    &session_request.job_ready_text(&ack),
                                ) {
                                    eprintln!(
                                        "agentnoise: failed to send job session ready message: {error:#}"
                                    );
                                }
                                group = new_group_id;
                            }
                            Err(error) => {
                                eprintln!(
                                    "agentnoise: failed to create job session chat: {error:#}"
                                );
                                let _ = dm.send_reply_to_blocking(
                                    &group_id,
                                    &format!(
                                        "agentnoise: failed to create a parallel session chat; running here.\n{error:#}"
                                    ),
                                );
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("agentnoise: failed to prepare job session chat: {error:#}");
                    }
                }
                let app_clone = Arc::clone(&app);
                let dm_clone = Arc::clone(&dm);
                let event_journal_clone = Arc::clone(&event_journal);
                let engine_clone = engine.clone();
                let account_id_clone = account_id_hex.clone();
                let handle_clone = tokio_handle.clone();
                thread::spawn(move || {
                    let job_id = uuid::Uuid::new_v4().to_string();
                    let started_at = agentnoise::dm_streams::current_unix_seconds();
                    let broker_endpoint = app_clone
                        .config()
                        .darkmatter
                        .agent_text_stream_broker
                        .clone();

                    let stream_result = agentnoise::dm_streams::AgentTextStream::start_blocking(
                        engine_clone,
                        account_id_clone,
                        &group,
                        &job_id,
                        started_at,
                        &broker_endpoint,
                        &handle_clone,
                    );
                    let stream_state = match stream_result {
                        Ok((stream, _summary)) => Some(stream),
                        Err(error) => {
                            eprintln!(
                                "agentnoise: failed to open agent text stream: {error:#}; \
                                 falling back to chunked replies"
                            );
                            None
                        }
                    };
                    let stream_cell: Arc<Mutex<Option<agentnoise::dm_streams::AgentTextStream>>> =
                        Arc::new(Mutex::new(stream_state));

                    let progress_dm = Arc::clone(&dm_clone);
                    let progress_group = group.clone();
                    let progress_stream = Arc::clone(&stream_cell);
                    let progress_journal = Arc::clone(&event_journal_clone);
                    let progress_handle = handle_clone.clone();
                    let progress_callback = move |text: String| {
                        let mut sent_to_stream = false;
                        if let Ok(mut slot) = progress_stream.lock()
                            && let Some(stream) = slot.as_mut()
                            && stream.live_preview_active()
                        {
                            match stream.append_text_blocking(&text, &progress_handle) {
                                Ok(report) => {
                                    if report.chunks_published > 0 {
                                        sent_to_stream = true;
                                    }
                                }
                                Err(error) => {
                                    eprintln!(
                                        "agentnoise: failed to publish stream progress: {error:#}; \
                                         falling back to final chat reply"
                                    );
                                }
                            }
                        }
                        if sent_to_stream {
                            return;
                        }
                        if progress_stream
                            .lock()
                            .map(|slot| slot.is_some())
                            .unwrap_or(false)
                        {
                            return;
                        }
                        if let Err(error) =
                            progress_dm.send_reply_to_blocking(&progress_group, &text)
                        {
                            eprintln!("agentnoise: failed to send progress: {error:#}");
                            return;
                        }
                        if let Ok(mut journal) = progress_journal.lock() {
                            let _ = journal.record_outbound(&progress_group, &text, true, None);
                        }
                    };

                    let result = app_clone.run_request_with_progress(request, progress_callback);
                    let reply = result.unwrap_or_else(|err| format!("Error: {err:#}"));

                    let mut stream_finalized = false;
                    if let Ok(mut slot) = stream_cell.lock()
                        && let Some(mut stream) = slot.take()
                    {
                        if stream.live_preview_active() {
                            match stream.append_text_blocking(&reply, &handle_clone) {
                                Ok(_report) => {}
                                Err(error) => {
                                    eprintln!(
                                        "agentnoise: failed to publish final stream chunk: {error:#}"
                                    );
                                }
                            }
                        }
                        let finished_at = agentnoise::dm_streams::current_unix_seconds();
                        match stream.finish_blocking(reply.clone(), finished_at, &handle_clone) {
                            Ok(_) => {
                                stream_finalized = true;
                            }
                            Err(error) => {
                                eprintln!(
                                    "agentnoise: failed to finalize agent text stream: {error:#}"
                                );
                            }
                        }
                    }

                    if !should_send_plain_final_reply(stream_finalized) {
                        return;
                    }
                    if let Err(error) = dm_clone.send_reply_to_blocking(&group, &reply) {
                        eprintln!("agentnoise: failed to send job reply: {error:#}");
                        return;
                    }
                    if let Ok(mut journal) = event_journal_clone.lock() {
                        let _ = journal.record_outbound(&group, &reply, true, None);
                    }
                });
            }
        }
    }

    tokio_handle.block_on(engine.shutdown());
    drop(tokio_runtime);
    Ok(())
}

fn should_send_plain_final_reply(stream_finalized: bool) -> bool {
    // The Final MLS envelope (AgentStreamFinalized) is the canonical record and
    // is always published when the stream finalizes; only fall back to a plain
    // chat reply when no durable Final envelope was published.
    !stream_finalized
}

struct GroupSubscriptionContext<'a> {
    tokio_handle: &'a tokio::runtime::Handle,
    dm: &'a Arc<DmClient>,
    tx: &'a mpsc::Sender<agentnoise::dm::MessageEvent>,
    subscribed_groups: &'a Arc<Mutex<HashSet<String>>>,
}

struct SessionGroupContext<'a> {
    config_path: &'a Path,
    app: &'a AgentApp,
    engine: &'a DarkmatterEngine,
    account_id_hex: &'a str,
    subscription: GroupSubscriptionContext<'a>,
}

fn activate_darkmatter_session_group(
    context: &SessionGroupContext<'_>,
    request: &NewSessionRequest,
) -> Result<String> {
    let group_id_hex = create_darkmatter_session_group(
        context.subscription.tokio_handle,
        context.engine,
        context.account_id_hex,
        request,
    )?;
    context
        .app
        .create_session_record(&group_id_hex, request.state.clone())?;
    persist_discovered_group(context.config_path, &group_id_hex)?;
    subscribe_group_if_new(&context.subscription, &group_id_hex);
    Ok(group_id_hex)
}

fn create_darkmatter_session_group(
    tokio_handle: &tokio::runtime::Handle,
    engine: &DarkmatterEngine,
    account_id_hex: &str,
    request: &NewSessionRequest,
) -> Result<String> {
    let members = vec![request.sender.clone()];
    let group_id = tokio_handle
        .block_on(engine.runtime().create_group(
            account_id_hex,
            &request.group_name,
            &members,
            None,
        ))
        .map_err(|err| anyhow::anyhow!("darkmatter create_group: {err}"))?;
    Ok(hex::encode(group_id.as_slice()))
}

fn subscribe_group_if_new(context: &GroupSubscriptionContext<'_>, group_id_hex: &str) -> bool {
    let inserted = match context.subscribed_groups.lock() {
        Ok(mut subscribed_groups) => subscribed_groups.insert(group_id_hex.to_string()),
        Err(_) => {
            eprintln!("agentnoise: group subscription lock poisoned");
            false
        }
    };
    if !inserted {
        return false;
    }
    spawn_group_subscription(
        context.tokio_handle,
        Arc::clone(context.dm),
        group_id_hex.to_string(),
        context.tx.clone(),
    );
    true
}

fn spawn_group_subscription(
    tokio_handle: &tokio::runtime::Handle,
    dm: Arc<DmClient>,
    group_id: String,
    tx: mpsc::Sender<agentnoise::dm::MessageEvent>,
) {
    tokio_handle.spawn(async move {
        let mut subscription = match dm.subscribe_group(&group_id).await {
            Ok(subscription) => subscription,
            Err(error) => {
                eprintln!("agentnoise: failed to subscribe to {group_id}: {error:#}");
                return;
            }
        };
        for event in subscription.snapshot() {
            if tx.send(event).is_err() {
                return;
            }
        }
        while let Some(event) = subscription.next_message().await {
            if tx.send(event).is_err() {
                return;
            }
        }
    });
}

fn spawn_group_join_discovery(
    tokio_handle: &tokio::runtime::Handle,
    engine: DarkmatterEngine,
    dm: Arc<DmClient>,
    account_id_hex: String,
    config_path: PathBuf,
    subscribed_groups: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<agentnoise::dm::MessageEvent>,
) {
    let mut events = engine.runtime().subscribe();
    let subscription_handle = tokio_handle.clone();
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
            if !subscribe_group_if_new(
                &GroupSubscriptionContext {
                    tokio_handle: &subscription_handle,
                    dm: &dm,
                    tx: &tx,
                    subscribed_groups: &subscribed_groups,
                },
                &group_id_hex,
            ) {
                continue;
            }
            eprintln!("agentnoise: joined group {group_id_hex} via darkmatter welcome");
            if let Err(error) = persist_discovered_group(&config_path, &group_id_hex) {
                eprintln!(
                    "agentnoise: failed to persist discovered group {group_id_hex}: {error:#}"
                );
            }
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

    #[test]
    fn plain_final_reply_is_only_stream_fallback() {
        // The plain reply is sent only when the stream did not finalize (no
        // durable Final envelope); a finalized stream is the canonical record.
        assert!(!should_send_plain_final_reply(true));
        assert!(should_send_plain_final_reply(false));
    }
}
