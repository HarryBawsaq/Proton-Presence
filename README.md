# proton-presence

Discord Rich Presence for Steam games running through Proton on Linux.

Discord on Linux usually cannot tell which game is running inside Proton, so
a game like Victoria 3 never shows up on your profile. proton-presence is a
small background service that fixes that. It finds the running Steam game,
looks up Discord's own entry for it, and tells Discord, so your profile shows
"Playing Victoria 3" with the game's icon and a timer.

Install it once and forget it. It starts at login, waits quietly while
Discord is closed, and reconnects on its own when Discord starts or restarts.
There is no Discord developer setup and nothing to configure.

Written for CachyOS; it works on any distribution with systemd, curl and a
current Rust toolchain.

## Setup (once)

Install Rust and git if you do not have them:

```bash
sudo pacman -S --needed rust git
```

Download the source:

```bash
git clone https://github.com/HarryBawsaq/Proton-Presence.git
```

```bash
cd Proton-Presence
```

Run the installer as your normal user (not with `sudo`):

```bash
./install.sh
```

That is all. The installer builds the release binary, copies it to
`~/.local/bin/proton-presence`, installs the systemd user unit to
`~/.config/systemd/user/proton-presence.service`, and enables and starts the
service. From then on it starts at every login.

### One-time steps in Discord

None are required. You do not need to create a Discord application, and
`config.toml` can stay as installed.

Only if you want a game that Discord does not know to show up as well, create
an application of your own and fill in `config.toml`; see
[Optional configuration](#optional-configuration).

### Check it

Start Discord, launch a Proton game, then run:

```bash
~/.local/bin/proton-presence status
```

This reports whether the service is running, whether Discord is reachable,
which game is detected, and which Discord application it will be shown under.

### Troubleshooting

Follow the service log:

```bash
journalctl --user -u proton-presence -f
```

Only state changes are logged: game started, game ended, connected,
disconnected. For more detail, set `Environment=RUST_LOG=debug` in
`~/.config/systemd/user/proton-presence.service`, then run:

```bash
systemctl --user daemon-reload
```

```bash
systemctl --user restart proton-presence
```

To see what would be sent, without connecting to Discord:

```bash
~/.local/bin/proton-presence --once
```

Common causes when nothing shows in Discord:

- **Settings → Activity Privacy → Share your detected activities with
  others** is off in Discord.
- Vesktop: Rich Presence (arRPC) has to be enabled in the Vesktop settings.
- The log says the game is not in Discord's game list. See
  [Optional configuration](#optional-configuration).
- The log says the game list cannot be downloaded. The service needs to
  reach `discord.com` once; it retries every ten minutes.

## What Discord shows

For a game Discord knows:

| Field | Value |
|---|---|
| Title and icon | The game's own, from Discord |
| State | `Playing via Proton` (`Playing on Steam` for a native Linux game) |
| Elapsed time | Counted from when the game was first detected |

The presence is cleared when the game exits.

## How it works

- **Detection.** Every 5 seconds it reads `/proc/<pid>/environ` of each
  process and looks for `SteamAppId` or `SteamGameId`. Launcher and runtime
  processes (`reaper`, `wineserver`, `steam-runtime-*`, `pressure-vessel-*`
  and Wine's own services) are ignored, so a lingering `wineserver` does not
  count as a running game.
- **Names.** The app ID is resolved from `appmanifest_<id>.acf` in the Steam
  libraries listed in `libraryfolders.vdf`, for the native Steam client
  (`~/.steam/steam`, `~/.local/share/Steam`) and the Flatpak one
  (`~/.var/app/com.valvesoftware.Steam`).
- **Matching.** Discord publishes the list of games it can detect, and each
  entry names the game's Steam app ID. proton-presence uses that to find the
  game's own Discord application.
- **Discord.** It connects to `discord-ipc-0` … `discord-ipc-9` in
  `$XDG_RUNTIME_DIR`, and in the Flatpak Discord, Flatpak Vesktop and Snap
  Discord subdirectories. The IPC protocol is implemented in
  `src/discord.rs`; there is no Discord library.
- **Reconnecting.** It only connects while a game is running. If Discord is
  closed it retries after 5, 10, 20, 40 and then every 60 seconds. If Discord
  quits mid-game the connection is dropped and re-established the same way,
  keeping the original start time.

### Network use

The only network request is downloading Discord's public game list
(`https://discord.com/api/v10/applications/detectable`, about 13 MB) with
`curl`. That happens when a game starts and the cached copy is missing or
older than a week. Nothing about you or your games is sent. The mapping is
cached in `~/.cache/proton-presence/discord-games.json`.

To switch this off, set `discord_game_list = false` in `config.toml`. Then
only client IDs from the config are used.

## Optional configuration

`~/.config/proton-presence/config.toml` is re-read automatically when it
changes. Everything in it is optional.

```toml
# Set to false to never download Discord's game list (default: true).
discord_game_list = true

# An application of your own, used for games Discord does not know.
default_client_id = "123456789012345678"

# Use an application of your own for one game, keyed by Steam app ID.
[games.1245620]
client_id = "234567890123456789"
large_image = "elden_ring"
```

For each game the first of these that applies is used:

1. `client_id` under `[games.<steam app id>]`
2. The game's own application from Discord's game list
3. `default_client_id`

If none applies, the game is not shown and the log says why.

To create an application of your own, open
<https://discord.com/developers/applications>, click **New Application**, and
copy its **Application ID**. Discord shows the application's name as
"Playing …", and the game's name on the line below. `large_image` is the key
of an image uploaded under **Rich Presence → Art Assets** in that
application; it is ignored for Discord's own applications, which already
have the game's icon.

## Uninstall

```bash
./uninstall.sh
```

This stops and disables the service and removes the unit, the binary and the
cached game list. Your config is kept; the script prints the command to
delete it.

## Development

```bash
cargo test
```

```bash
cargo clippy --all-targets -- -D warnings
```

The tests use a temporary directory as a fake `/proc`, a fake Discord server
on a Unix socket and a canned game list, so they never touch the real system
or the network.

## Limitations

- A game that is not in Discord's game list is only shown if you set
  `default_client_id`.
- Non-Steam shortcuts have no app manifest and are not in Discord's list, so
  with `default_client_id` they are shown as `Steam app <id>`.
- If two games run at once, the one detected first is shown until it exits.
