# Supervisor Services

agentnoise does not daemonize itself. The reboot-safe service is the transport:
it owns White Noise subscriptions, pairing, discovery, and the durable job
queue. Local agent execution runs from a login-shell worker.

`agentnoise up` is the human-facing entry point:

- If a supervisor already owns the engine, interactive `agentnoise up` attaches
  as a local UI and follows logs.
- If no engine is running, interactive `agentnoise up` takes the engine lock and
  runs transport plus jobs in the foreground.
- Non-interactive `agentnoise up` waits for the engine lock, which lets a
  service take over after a foreground troubleshooting run exits.

The service-facing entry point is `agentnoise transport run`. Start at least
one worker for `/codex`, `/claude`, `/wiki`, or `/hermes` jobs:

```sh
agentnoise worker start
# or, if tmux is installed, run the supervised restart loop:
agentnoise worker start --tmux
```

## macOS

Homebrew installs should use brew services:

```sh
brew services start nvk/tap/agentnoise
brew services stop nvk/tap/agentnoise
```

Development or non-Homebrew installs can use the native LaunchAgent wrapper:

```sh
agentnoise service install --target launchd --force --load
agentnoise service uninstall --target launchd --unload
```

`agentnoise launchd ...` remains as a macOS-specific compatibility command.

For multiple isolated helpers on one machine, install named instances instead
of sharing one service:

```sh
agentnoise --instance alice service install --target launchd --force --load
agentnoise --instance bob service install --target launchd --force --load
```

Those write separate LaunchAgents:

```text
~/Library/LaunchAgents/com.agentnoise.agentnoise.alice.plist
~/Library/LaunchAgents/com.agentnoise.agentnoise.bob.plist
```

Current Codex CLI releases do not run reliably when `codex exec` is launched
directly by launchd. They can start and then produce no output forever. The
macOS service therefore runs only `agentnoise transport run`; Codex jobs should
be claimed by a login-shell worker. On remote hosts, prefer the supervised
tmux worker so transient worker exits do not leave queued jobs stranded:

```sh
agentnoise worker start
```

If you intentionally want to test launchd-launched Codex anyway, set
`AGENTNOISE_ALLOW_LAUNCHD_CODEX=1` in that service environment.

## Linux

Use a user systemd service:

```sh
agentnoise service install --target systemd-user --force --load
agentnoise worker start
systemctl --user status agentnoise.service
```

The generated unit is written to:

```text
~/.config/systemd/user/agentnoise.service
```

Named instances write named units:

```sh
agentnoise --instance alice service install --target systemd-user --force --load
systemctl --user status agentnoise-alice.service
```

For packagers, start from:

```text
packaging/systemd/agentnoise.service
```

## FreeBSD

Use an rc.d service. Package maintainers can install:

```text
packaging/freebsd/agentnoise -> /usr/local/etc/rc.d/agentnoise
```

Then set:

```sh
sysrc agentnoise_enable=YES
sysrc agentnoise_user=<user>
service agentnoise start
```

The CLI can render or install the same shape:

```sh
agentnoise service print --target freebsd-rc
agentnoise service install --target freebsd-rc --force
```

Named instances use separate script names and rc variables, for example
`/usr/local/etc/rc.d/agentnoise-alice` with `agentnoise_alice_enable`.

## OpenBSD

Install the rc.d script:

```text
packaging/openbsd/agentnoise -> /etc/rc.d/agentnoise
```

Then:

```sh
rcctl enable agentnoise
rcctl start agentnoise
```

The CLI can also render it:

```sh
agentnoise service print --target openbsd-rc
```

Named instances render separate rc.d script names such as
`/etc/rc.d/agentnoise-alice`.

## First Pairing

For Homebrew installs, first pairing can run directly from the service:

```sh
agentnoise up --no-listen
brew services start nvk/tap/agentnoise
agentnoise worker start
tail -f "$(brew --prefix)/var/log/agentnoise.log"
```

For foreground troubleshooting, run `agentnoise up`. If `allowed_senders` is
empty and no service owns the engine, agentnoise prints the QR and rotating PIN.
On macOS it also shows the desktop identity QR, current PIN, and live countdown
while the PIN is valid. If the service is already running, the same command
attaches to the service instead of starting another listener. The process keeps
running before the first control chat exists and discovers the phone-created
chat automatically.

## Secret Storage

agentnoise stores the desktop White Noise `nsec` through the platform credential
store selected at build time, unless `--dev-burner-nsec` has explicitly enabled
a plaintext development burner file:

- macOS: Apple Keychain.
- Windows: Windows Credential Manager.
- Linux: kernel keyutils plus Secret Service persistence.
- FreeBSD/OpenBSD: DBus Secret Service.

On Linux and BSD, unattended restart depends on the same user session being able
to reach an unlocked Secret Service collection. Headless servers should run
`agentnoise keychain status` from the exact service account and supervisor
context before relying on restart repair.
