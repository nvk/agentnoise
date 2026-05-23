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
brew services start nvk/tap/agentnoise
agentnoise worker start
```

For disposable remote testing where keychain prompts get in the way:

```sh
agentnoise up --ssh --phone npub1... --name agentnoise-test --dev-burner-nsec
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

Keep the SSH terminal open until pairing completes, then stop the foreground
`agentnoise up` process. After that the service keeps transport alive and the
login-shell worker runs jobs:

```sh
agentnoise transport status
agentnoise worker status
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
profile `name` is also saved.

After setup, inspect or rename the current desktop identity without passing a
phone `npub`:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-labbox
```

`identity status` reads the stored public account from config when available,
so it does not need the desktop `nsec`. `identity rename` saves the new machine
label; the embedded Marmot v2 engine publishes the updated profile when the
listener starts (or via `--no-publish` to save only).

## Message Relays

The QR relay hints only help the phone find the desktop identity. Actual
message delivery uses the Marmot v2 account relay list. Inspect engine state
from SSH:

```sh
agentnoise darkmatter probe --relay wss://relay.primal.net
```

## Secret Handling

Do not pass an `nsec` over SSH for normal setup. If you need to import an
existing identity, pass it only through stdin to `agentnoise keychain
store-nsec`; never put it in an argument, environment variable, shell history,
or QR code.

For headless or unreliable keychain environments, `--dev-burner-nsec` uses a
local `0600` plaintext file under the agentnoise data dir. It is practical for
throwaway remote testing, not for a valuable long-lived identity.
