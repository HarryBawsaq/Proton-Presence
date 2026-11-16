# proton-presence

Shows the Steam game you are playing through Proton on your Discord profile.

Install it once and forget it. It starts at login, waits quietly while
Discord is closed, and reconnects on its own when Discord starts or restarts.
There is no Discord developer setup and nothing to configure.

It works on Arch-based, Debian-based and Fedora-based distributions; see
[Supported systems](#supported-systems).

## The problem it fixes

On Windows, Discord shows what you are playing. On Linux, with a game running
through Proton, it usually shows nothing. Victoria 3 is a typical example:
launch it through Proton and your Discord profile stays empty.

The reason is how Discord notices games. It recognises them by looking at the
programs running on your computer. A Proton game is a Windows program running
inside Wine, which does not look like anything Discord knows on Linux. So
your profile stays empty, no matter which game it is. This affects most
Windows games played through Proton.

proton-presence fills that gap. It detects the running Steam game itself and
tells Discord, using Discord's own entry for that game, so the real title and
icon appear.

## What Discord shows

For a game Discord knows:

| Field | Value |
|---|---|
| Title and icon | The game's own, from Discord |
| Elapsed time | Counted from when the game was first detected |

The presence is cleared when the game exits.

### What it does not show: in-game details

On Windows, some games also tell Discord what is happening inside the game,
such as which country you are playing and in which year. proton-presence does
not show that.

Those details come from the game itself. The game sends them through a
Windows-only channel that does not exist inside Proton, so they never reach
Discord. Passing them on means running an extra program inside the game's
Wine environment. An earlier version of proton-presence did that, and it was
removed: on real systems it could keep a game from closing after you quit,
and it did not reliably deliver the details either. The service now only
watches which game is running and never starts anything alongside it.

## Supported systems

| Family | Examples | Notes |
|---|---|---|
| Arch-based | Arch, CachyOS, EndeavourOS, Manjaro | |
| Debian-based | Debian 13, Ubuntu 24.04, Linux Mint 22, Pop!_OS 24.04, and newer | Older releases ship a Rust that is too old; see below |
| Fedora-based | Fedora 41 and newer, Nobara | |

What it needs is the same everywhere: systemd (for the background service),
`curl`, `git`, and Rust 1.75 or newer to build it. Nothing in the program is
specific to one distribution.

Steam and Discord are found however they were installed:

| | Works with |
|---|---|
| Steam | The distribution's package, Valve's `.deb`, Flatpak, Snap |
| Discord | The distribution's package, Discord's `.deb` or `.tar.gz`, Flatpak, Snap, Vesktop |

It has been developed on and for CachyOS. On Debian and Fedora the build has
been checked against the Rust versions those distributions ship, but it has
not been run on real installations of them. Image-based systems (Bazzite,
Fedora Silverblue, SteamOS) are untested.

## Setup (once)

### 1. Install what the build needs

Arch, CachyOS, EndeavourOS, Manjaro:

```bash
sudo pacman -S --needed rust git curl
```

Debian 13 or newer, Ubuntu 24.04 or newer, Linux Mint 22 or newer, Pop!_OS:

```bash
sudo apt install cargo git curl
```

Fedora, Nobara:

```bash
sudo dnf install cargo git curl
```

On an older Debian or Ubuntu release (Debian 12, Ubuntu 22.04, Mint 21), the
packaged Rust is too old. Install `git`, `curl` and a C toolchain from the
distribution and Rust from <https://rustup.rs> instead:

```bash
sudo apt install git curl build-essential
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then open a new terminal so that `cargo` is found.

### 2. Download and install

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

That is all, on every distribution. The installer builds the release binary,
copies it to `~/.local/bin/proton-presence`, installs the systemd user unit
to `~/.config/systemd/user/proton-presence.service`, and enables and starts
the service. From then on it starts at every login.

After updating (`git pull`), run `./install.sh` again.

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
disconnected. A normal session looks like this:

```
Game started: Victoria 3 (app 529340)
Connected to Discord at /run/user/1000/discord-ipc-0 (client ID 1124352195157045371)
Game ended: Victoria 3 (app 529340)
Presence cleared, disconnected from Discord
```

For more detail, set `Environment=RUST_LOG=debug` in
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

## How it works

- **Detection.** Every 5 seconds it reads `/proc/<pid>/environ` of each
  process and looks for `SteamAppId` or `SteamGameId`. Launcher and runtime
  processes (`reaper`, `wineserver`, `steam-runtime-*`, `pressure-vessel-*`
  and Wine's own services) are ignored, so a lingering `wineserver` does not
  count as a running game.
- **Names.** The app ID is resolved from `appmanifest_<id>.acf` in the Steam
  libraries listed in `libraryfolders.vdf`.
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

The service only observes. It reads `/proc` and Steam's files, and talks to
Discord. It does not start, modify or attach to anything in a game or in its
Wine environment, so it cannot affect how a game runs or exits.

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

The minimum Rust version is 1.75 (set as `rust-version` in `Cargo.toml`), so
that the Rust packaged by Ubuntu 24.04 can build it. `Cargo.lock` pins
dependency versions that build with it.

## Limitations

- In-game details are not shown; see
  [What it does not show](#what-it-does-not-show-in-game-details).
- A game that is not in Discord's game list is only shown if you set
  `default_client_id`.
- Non-Steam shortcuts have no app manifest and are not in Discord's list, so
  with `default_client_id` they are shown as `Steam app <id>`.
- If two games run at once, the one detected first is shown until it exits.
