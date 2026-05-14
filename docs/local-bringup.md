# Local Bring-Up

This is the normal desktop-to-phone path. Run it from a regular user terminal,
not from a restricted agent sandbox, because setup needs the macOS login
keychain and `~/Library/Application Support`.

## Build

```sh
cargo build --release
export AGENTNOISE="$PWD/target/release/agentnoise"
```

## Pair

If you know the phone White Noise `npub`:

```sh
"$AGENTNOISE" up --phone npub...
```

That creates or reuses the desktop keypair in the OS keychain, writes the config,
starts the White Noise daemon if needed, logs in from the keychain, publishes the
desktop profile/key package, creates the `agentnoise` control chat, and saves
the group id when `wn` returns it.

If you do not know the phone `npub`:

```sh
"$AGENTNOISE" up
```

Scan the QR in White Noise, create a chat/group with the desktop identity, then
leave the process running. It will keep discovering White Noise chats until the
new chat appears. If you used `--no-listen` or stopped the process, run the same
command again:

```sh
"$AGENTNOISE" up
```

If `allowed_senders` is still empty, `up` prints a QR and a 6-digit PIN. On
macOS it also opens a pairing window with the QR, desktop `npub`, PIN, and live
countdown. Send the PIN from the phone as the first message.
agentnoise ignores every other message until the PIN succeeds, then saves that
sender to `allowed_senders`.

## Run

```sh
"$AGENTNOISE" up
```

From the phone, send:

```text
/status
/repos
/use sandbox
/pwd
/codex say hello
/wiki research agent chat ux
```

Each White Noise chat with the agentnoise desktop identity is an independent
agentnoise session. You can create another chat from the same phone identity,
send `/use` or `/cd` there, and it will not disturb the workspace state in the
first chat.

When the foreground test works:

```sh
"$AGENTNOISE" service install --target launchd --force --load
```

## Permission Note

`codex-fix` and `codex-unsafe` are enough for editing and testing agentnoise.
They are not enough for local bring-up, because those `nono` profiles still
block the macOS keychain and default Application Support paths that White Noise
uses. Use a normal terminal, or a no-`nono` profile such as `codex-rawdog`, for
the actual setup run.
