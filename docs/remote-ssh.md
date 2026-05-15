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

For disposable remote testing where keychain prompts get in the way:

```sh
agentnoise up --ssh --phone npub1... --name agentnoise-test --dev-burner-nsec
```

What happens:

1. agentnoise creates or reuses a desktop keypair on the remote machine.
2. The desktop `nsec` stays on the remote machine.
3. The phone `npub` is used to create the White Noise control chat.
4. The pairing PIN is printed in the SSH terminal.
5. No desktop GUI alert is opened in `--ssh` mode.
6. The phone sends `/pair 123456` in that White Noise chat.
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

The display name is saved in `config.toml` and published through the White
Noise/Nostr profile. The normalized Nostr profile `name` is also saved.

After setup, inspect or rename the current desktop identity without passing a
phone `npub`:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-labbox
```

`identity status` reads the stored public account from config when available,
so it does not need the desktop `nsec`. `identity rename` saves the new machine
label and publishes it through White Noise; use `--no-publish` to save config
only and publish on the next `agentnoise up`.

## Message Relays

The QR relay hints only help the phone find the desktop identity. Actual
message delivery uses the White Noise account relay list. Check or repair that
state from SSH with:

```sh
agentnoise whitenoise relays
agentnoise whitenoise ensure-relays
```

## Secret Handling

Do not pass an `nsec` over SSH for normal setup. If you need to import an
existing identity, pass it only through stdin to `agentnoise keychain
store-nsec`; never put it in an argument, environment variable, shell history,
or QR code.

For headless or unreliable keychain environments, `--dev-burner-nsec` uses a
local `0600` plaintext file under the agentnoise data dir. It is practical for
throwaway remote testing, not for a valuable long-lived identity.
