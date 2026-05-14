# Supervisor Services

agentnoise does not daemonize itself. The engine is a foreground process owned
by either a supervisor or the terminal. The host supervisor owns boot, restart,
stop, and logs.

`agentnoise up` is the human-facing entry point:

- If a supervisor already owns the engine, interactive `agentnoise up` attaches
  as a local UI and follows logs.
- If no engine is running, interactive `agentnoise up` takes the engine lock and
  runs in the foreground.
- Non-interactive `agentnoise up` waits for the engine lock, which lets a
  service take over after a foreground troubleshooting run exits.

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

## Linux

Use a user systemd service:

```sh
agentnoise service install --target systemd-user --force --load
systemctl --user status agentnoise.service
```

The generated unit is written to:

```text
~/.config/systemd/user/agentnoise.service
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

## First Pairing

For Homebrew installs, first pairing can run directly from the service:

```sh
brew services start nvk/tap/agentnoise
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
