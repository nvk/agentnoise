# Supervisor Services

AgentNoise does not daemonize itself. `agentnoise up` is a foreground process:
it starts or repairs White Noise, listens, logs to stdout/stderr, and exits on a
fatal error. The host supervisor owns boot, restart, stop, and logs.

## macOS

Homebrew installs should use brew services:

```sh
brew services start agentnoise
brew services stop agentnoise
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

Run first pairing in the foreground when possible:

```sh
agentnoise up
```

If `allowed_senders` is empty, AgentNoise prints the QR and rotating PIN there.
On macOS it also shows the desktop identity QR, current PIN, and live countdown
while the PIN is valid. After the phone sends the PIN and the sender is saved,
start the service. If a service starts before pairing, the PIN is printed to
that supervisor's log.

## Secret Storage

AgentNoise stores the desktop White Noise `nsec` through the platform credential
store selected at build time:

- macOS: Apple Keychain.
- Windows: Windows Credential Manager.
- Linux: kernel keyutils plus Secret Service persistence.
- FreeBSD/OpenBSD: DBus Secret Service.

On Linux and BSD, unattended restart depends on the same user session being able
to reach an unlocked Secret Service collection. Headless servers should run
`agentnoise keychain status` from the exact service account and supervisor
context before relying on restart repair.
