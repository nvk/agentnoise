# Launchd Service

agentnoise can install itself as a per-user macOS LaunchAgent.

## Install

Run setup first:

```sh
agentnoise up --no-listen
agentnoise doctor
```

Install and load:

```sh
agentnoise service install --target launchd --force --load
```

The older macOS-specific command is still available:

```sh
agentnoise launchd install --force --load
```

The plist is written to:

```text
~/Library/LaunchAgents/com.agentnoise.agentnoise.plist
```

Logs are written under:

```text
~/Library/Logs/agentnoise/
```

After loading the service, `agentnoise up` from a terminal attaches to the
running LaunchAgent instead of starting a second listener. If the LaunchAgent is
not running, the same command takes the foreground engine lock for
troubleshooting.

## Inspect

```sh
agentnoise service print --target launchd
launchctl print "gui/$(id -u)/com.agentnoise.agentnoise"
```

## Uninstall

```sh
agentnoise service uninstall --target launchd --unload
```

## Operational Notes

- Run `agentnoise doctor` before loading the service.
- Keep `allowed_senders` non-empty for unattended service use. If it is empty,
  `agentnoise up` enters PIN pairing mode. On macOS it shows the desktop
  identity QR, current PIN, and live countdown, and also prints the PIN to the
  launchd log.
- `launchd install` runs `agentnoise up`, which starts `wnd`, repairs login from the configured bootstrap nsec when needed, enforces first-pairing PIN auth, waits for the first control chat when needed, then listens.
- Restart the service after changing config.
- If the process restarts, active jobs are marked `interrupted` in the local job store.
