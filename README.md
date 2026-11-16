# proton-presence

Discord Rich Presence for Steam games running through Proton on Linux.

Install it once and forget it. It starts at login, waits quietly while
Discord is closed, and reconnects on its own when Discord starts or restarts.
There is no Discord developer setup and nothing to configure.

It works on Arch-based, Debian-based and Fedora-based distributions; see
[Supported systems](#supported-systems).

## The problem it fixes

On Windows, Discord shows what you are playing, and for many games also what
you are doing in the game. On Linux, with a game running through Proton, both
of those usually break. Victoria 3 is a typical example: launch it through
Proton and your Discord profile shows nothing at all.

There are two separate reasons.

**1. Discord does not notice the game.** Discord recognises games by looking
at the programs running on your computer. A Proton game is a Windows program
running inside Wine, which does not look like anything Discord knows on
Linux. So your profile stays empty, no matter which game it is. This affects
most Windows games played through Proton.

**2. The game cannot reach Discord.** Some games have Rich Presence built in:
they tell Discord what is going on, such as which country you are playing and
in which year. The game sends this through a Windows-only channel (a named
pipe called `discord-ipc-0`). Inside Proton that channel does not exist, so
the message goes nowhere, even though Discord is running right next to it.

proton-presence fixes both:

- It detects the running Steam game itself and tells Discord, using
  Discord's own entry for that game, so the real title and icon appear.
- It starts a small helper next to each Proton game that provides the
  missing channel and passes the game's messages on to Discord. If the game
  reports in-game details, Discord shows exactly those.

## What Discord shows

| The game | What appears on your profile |
|---|---|
| Reports in-game details | Whatever the game reports, for example the country and year in Victoria 3, exactly as on Windows |
| Does not report anything | The game's title and icon, and how long you have been playing |

Which of the two you get depends only on the game. Not every game has Rich
Presence built in, and some only report details in certain situations (in a
match, but not in the menu). While a game reports nothing, the title and play
time are shown; as soon as it reports something, that takes over.

The presence is cleared when the game exits.

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

One exception: passing on in-game details needs Steam installed as a normal
package. With Flatpak or Snap Steam, games still show their title and play
time.

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
copies it to
`~/.local/bin/proton-presence`, copies the helper to
`~/.local/share/proton-presence/`, installs the systemd user unit to
`~/.config/systemd/user/proton-presence.service`, and enables and starts the
service. From then on it starts at every login.

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

Only state changes are logged. For a game with built-in Rich Presence a
normal session looks like this:

```
Game started: Victoria 3 (app 529340)
Connected to Discord at /run/user/1000/discord-ipc-0 (client ID 1124352195157045371)
Ready to pass on in-game details of Victoria 3
The game opened its own connection to Discord (application 123456789012345678)
Victoria 3 reports its own rich presence; showing that
Presence cleared, disconnected from Discord
Game ended: Victoria 3 (app 529340)
```

How far a session gets shows where in-game details stop:

| Last of these lines in the log | Meaning |
|---|---|
| `Connected to Discord` | The helper did not start next to the game. A warning says why. |
| `Ready to pass on in-game details` | The helper is running, but the game never looked for Discord. Either it has no Rich Presence, or it looked before the helper was up (see [Limitations](#limitations)). |
| `The game opened its own connection` | The game talks to Discord but has not reported anything to show. Some games only do so in certain situations. A warning that Discord rejected a request also appears here. |
| `reports its own rich presence` | Everything works; Discord is showing what the game sent. |

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

Common causes when something is missing:

- Nothing shows at all: **Settings → Activity Privacy → Share your detected
  activities with others** is off in Discord.
- Nothing shows at all with Vesktop: Rich Presence (arRPC) has to be enabled
  in the Vesktop settings.
- The log says the game is not in Discord's game list. See
  [Optional configuration](#optional-configuration).
- The log says the game list cannot be downloaded. The service needs to
  reach `discord.com` once; it retries every ten minutes.
- The log says in-game details cannot be passed on. The helper could not be
  started next to the game; the title and play time are still shown.
- The title shows but never any details: see the table above.

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
- **In-game details.** When a Proton game starts, the service runs a small
  Windows program, `proton-presence-bridge.exe`, in the game's Wine prefix,
  using the same Wine and the same environment as the game (entering Steam's
  container with `nsenter` where there is one). The helper creates the
  `discord-ipc-0` named pipe the game is looking for and relays whatever the
  game sends to the service over TCP on `127.0.0.1`; the service passes it on
  to Discord unchanged. While the game holds a presence of its own, the
  service withdraws the basic one so only one is shown. The helper is a
  separate process: it does not modify or inject anything into the game.
- **Never holding a game open.** Wine does not shut a prefix down while a
  program is running in it, and Steam shows a game as running until its
  prefix is gone. So the helper registers with Wine as a background system
  process, like Wine's own services: Wine does not wait for those, and tells
  them to exit when the last real program has gone. If the helper cannot
  register that way it does not run at all. Independently of that, the
  service dismisses the helper once no Windows program of the game is left,
  and stops it by force if it is still there one poll later.
- **Reconnecting.** If Discord is closed the service retries after 5, 10,
  20, 40 and then every 60 seconds. If Discord quits mid-game the connection
  is dropped and re-established the same way, keeping the original start
  time.

### Network use

The only request to the internet is downloading Discord's public game list
(`https://discord.com/api/v10/applications/detectable`, about 13 MB) with
`curl`. That happens when a game starts and the cached copy is missing or
older than a week. Nothing about you or your games is sent. The mapping is
cached in `~/.cache/proton-presence/discord-games.json`.

The helper talks to the service through a port on `127.0.0.1` that is only
open while the service runs. It is not reachable from other computers, and
connections must present a random token that only the helper is given.

## Optional configuration

`~/.config/proton-presence/config.toml` is re-read automatically when it
changes. Everything in it is optional.

```toml
# Set to false to never download Discord's game list (default: true).
discord_game_list = true

# Set to false to never start the helper next to games (default: true).
# Games then show their title and play time only.
game_rich_presence = true

# An application of your own, used for games Discord does not know.
default_client_id = "123456789012345678"

# Use an application of your own for one game, keyed by Steam app ID.
[games.1245620]
client_id = "234567890123456789"
large_image = "elden_ring"
```

For each game the first of these that applies is used for the basic
presence:

1. `client_id` under `[games.<steam app id>]`
2. The game's own application from Discord's game list
3. `default_client_id`

If none applies, the game is not shown and the log says why. A game's own
Rich Presence does not depend on any of this.

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

This stops and disables the service and removes the unit, the binary, the
helper and the cached game list. Your config is kept; the script prints the
command to delete it.

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

The helper's source is `bridge/bridge.c`. The repository also contains the
compiled `bridge/proton-presence-bridge.exe`, so that installing does not
need a Windows cross-compiler. To build it yourself, install the mingw-w64
compiler (`mingw-w64-gcc` on Arch, `gcc-mingw-w64-x86-64` on Debian and
Ubuntu, `mingw64-gcc` on Fedora) and run:

```bash
bridge/build.sh
```

The build is reproducible: the same compiler produces the same file.
`install.sh` rebuilds the helper automatically when the compiler is present.

## Limitations

- In-game details appear only for games that have Discord Rich Presence
  built in, and only when the game chooses to report them.
- The helper is started within about five seconds of the game. A game that
  looks for Discord only once, in its first seconds, and starts without a
  launcher can miss it; it then shows its title and play time for that
  session.
- A game that is not in Discord's game list is only shown if you set
  `default_client_id`.
- Steam installed as a Flatpak or Snap cannot see the helper, so games show
  their title and play time only.
- If another Discord bridge is already set up in a game's Wine prefix, the
  helper leaves it alone. Remove the other bridge to avoid a double entry.
- Non-Steam shortcuts have no app manifest and are not in Discord's list, so
  with `default_client_id` they are shown as `Steam app <id>`.
- If two games run at once, the one detected first is shown until it exits.
