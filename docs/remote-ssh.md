# Remote SSH Pairing

Use this when the agentnoise machine is reachable only over SSH.

The important rule: pass the phone `npub`, never an `nsec`. The remote machine
generates its own desktop agentnoise identity locally and stores that secret on
the remote machine.

## First Pairing

From the SSH session:

```sh
brew install nvk/tap/agentnoise
brew services stop nvk/tap/agentnoise
agentnoise up --ssh --phone npub1... --name agentnoise-mbp
```

What happens:

1. agentnoise creates or reuses a desktop keypair on the remote machine.
2. The desktop `nsec` stays on the remote machine.
3. The phone `npub` is a hint for QR/discovery — under Marmot v2 the phone
   creates the control group itself; the desktop joins via the welcome event.
4. The pairing PIN is printed in the SSH terminal.
5. No desktop GUI alert is opened in `--ssh` mode.
6. The phone sends `/pair 123456` in that Marmot v2 chat.
7. agentnoise stores the allowed sender and starts accepting commands.

Keep the SSH terminal open until pairing completes. After pairing works, leave
the foreground process running or move it back to the service:

```sh
brew services restart nvk/tap/agentnoise
```

## Naming Machines

Use a distinct `--name` per machine so the phone can tell multiple agentnoise
identities apart:

```sh
agentnoise up --ssh --phone npub1... --name agentnoise-mbp
agentnoise up --ssh --phone npub1... --name agentnoise-linuxbox
agentnoise up --ssh --phone npub1... --name agentnoise-freebsd
```

The display name is saved in `config.toml` and published as the Nostr profile
(kind 0) by the embedded Marmot v2 engine on next startup. The normalized
profile `name` is also saved. `identity rename` publishes immediately when a
local desktop account already exists; pass `--no-publish` to save config only.

After setup, inspect or rename the current desktop identity without passing a
phone `npub`:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-labbox
```

`identity status` reads the stored public account from config when available,
so it does not need the desktop `nsec`.

## Message Relays

The QR relay hints only help the phone find the desktop identity. Actual
message delivery uses the Marmot v2 account relay list. Inspect engine state
from SSH:

```sh
agentnoise darkmatter probe --relay wss://relay.primal.net
```

## Secret Handling

Do not pass an `nsec` over SSH for normal setup. agentnoise creates a remote
desktop identity locally and stores it through the platform credential store.
If the remote host cannot provide an unlocked keychain or Secret Service for the
service account, run the listener in the foreground user session or fix that
credential-store access before relying on unattended restart.
