# proton-presence

Discord Rich Presence for Steam games running through Proton on Linux.

Install it once and forget it. It starts at login, waits quietly while
Discord is closed, and reconnects on its own when Discord starts or restarts.
There is no Discord developer setup and nothing to configure.

Written for CachyOS; it works on any distribution with systemd, curl and a
current Rust toolchain.

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
Victoria 3 reports its own rich presence; showing that
Presence cleared, disconnected from Discord
Game ended: Victoria 3 (app 529340)
```

The third line means the helper is running next to the game. The fourth
appears once the game actually reports something; a game without Rich
Presence never gets there, and keeps showing its title and play time.

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
- The title shows but never any details, and the log has no warning: the
  game does not report any, or not in the situation you are in.

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
  separate process: it does not modify or inject anything into the game, and
  it exits when the game does.
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

The helper's source is `bridge/bridge.c`. The repository also contains the
compiled `bridge/proton-presence-bridge.exe`, so that installing does not
need a Windows cross-compiler. To build it yourself, install `mingw-w64-gcc`
and run:

```bash
bridge/build.sh
```

The build is reproducible: the same compiler produces the same file.
`install.sh` rebuilds the helper automatically when the compiler is present.

## Limitations

- In-game details appear only for games that have Discord Rich Presence
  built in, and only when the game chooses to report them.
- A game that is not in Discord's game list is only shown if you set
  `default_client_id`.
- Steam installed as a Flatpak cannot see the helper, so games show their
  title and play time only.
- If another Discord bridge is already set up in a game's Wine prefix, the
  helper leaves it alone. Remove the other bridge to avoid a double entry.
- Non-Steam shortcuts have no app manifest and are not in Discord's list, so
  with `default_client_id` they are shown as `Steam app <id>`.
- If two games run at once, the one detected first is shown until it exits.
