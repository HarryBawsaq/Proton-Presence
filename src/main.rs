//! proton-presence: Discord Rich Presence for Steam games running through
//! Proton. Runs as a systemd user service and polls `/proc` every 5 seconds.

mod bridge;
mod catalog;
mod config;
mod daemon;
mod detect;
mod discord;
mod logger;
mod steam;
#[cfg(test)]
mod testutil;
mod vdf;

use std::env;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Instant;

use anyhow::Result;

use crate::catalog::Lookup;
use crate::config::{Config, Resolved, Source};
use crate::daemon::{Daemon, Game, Paths, POLL_INTERVAL};

const SERVICE: &str = "proton-presence.service";

const USAGE: &str = "\
Discord Rich Presence for Steam games running through Proton.

Usage:
  proton-presence            Run the daemon (normally started by systemd)
  proton-presence status     Show service, Discord and game status
  proton-presence --once     Print the detected game and the activity
                             payload without connecting to Discord
  proton-presence --help     Show this help
  proton-presence --version  Show the version

Environment:
  RUST_LOG   Log level: error, warn, info (default), debug, trace

Games are matched to Discord's own applications using Discord's public game
list, downloaded with curl about once a week. Games that report in-game
details get a small helper started in their Wine prefix to pass them on.
A config file is optional: ~/.config/proton-presence/config.toml
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        [] => run(),
        ["status"] => status(),
        ["--once"] => once(),
        ["-h" | "--help" | "help"] => {
            print!("{USAGE}");
            Ok(())
        }
        ["-V" | "--version"] => {
            println!("proton-presence {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("proton-presence: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    logger::init();
    let paths = Paths::from_env()?;
    log::info!(
        "proton-presence {} started, polling every {}s",
        env!("CARGO_PKG_VERSION"),
        POLL_INTERVAL.as_secs()
    );
    let mut daemon = Daemon::new(
        paths,
        Box::new(catalog::curl_fetch),
        Box::new(bridge::SystemRunner),
    );
    loop {
        daemon.tick(Instant::now(), daemon::unix_ms());
        thread::sleep(POLL_INTERVAL);
    }
}

fn describe_game(game: &Game) -> String {
    let how = if game.proton { "Proton" } else { "native" };
    format!(
        "{} (app {}, pid {}, {how})",
        game.name, game.app_id, game.pid
    )
}

fn describe_client(config: &Config, game: &Game) -> String {
    match config.resolve(game.app_id, game.official_id()) {
        Some(r) => match r.source {
            Source::Mapped => format!(
                "{} (from [games.{}] in the config)",
                r.client_id, game.app_id
            ),
            Source::Official => {
                format!("{} (Discord's own application for this game)", r.client_id)
            }
            Source::Default => format!("{} (default_client_id from the config)", r.client_id),
        },
        None if !config.discord_game_list => {
            "none: discord_game_list is off and the config has no client ID for this game".into()
        }
        None if game.official == Lookup::Unavailable => {
            "not known yet: the service downloads Discord's game list when a game starts".into()
        }
        None => {
            "none: this game is not in Discord's game list and default_client_id is not set".into()
        }
    }
}

/// Runs `systemctl --user <verb> proton-presence.service` and returns the
/// one-word state it prints.
fn systemctl(verb: &str) -> Option<String> {
    let output = Command::new("systemctl")
        .args(["--user", verb, SERVICE])
        .output()
        .ok()?;
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!state.is_empty()).then_some(state)
}

fn status() -> Result<()> {
    let paths = Paths::from_env()?;

    let service = match (systemctl("is-active"), systemctl("is-enabled")) {
        (None, _) => "unknown (systemctl --user is not available)".to_string(),
        (Some(active), enabled) => {
            let running = if active == "active" {
                "running".to_string()
            } else {
                format!("not running ({active})")
            };
            match enabled.as_deref() {
                Some("enabled") => format!("{running}, starts at login"),
                Some(other) => format!("{running}, does not start at login ({other})"),
                None => running,
            }
        }
    };
    println!("Service:  {service}");

    match discord::probe(&paths.socket_dirs) {
        Some(socket) => println!("Discord:  reachable at {}", socket.display()),
        None => println!("Discord:  not reachable (not running, or its IPC socket is missing)"),
    }

    let config = Config::load(&paths.config_path);
    let game = daemon::detect_game(&paths, daemon::unix_ms());
    match &game {
        Some(game) => println!("Game:     {}", describe_game(game)),
        None => println!("Game:     none detected"),
    }
    if let (Some(game), Ok(config)) = (&game, &config) {
        println!("Client:   {}", describe_client(config, game));
    }

    if paths.bridge_helper.is_file() {
        println!("Details:  in-game details are passed on for games that report them");
    } else {
        println!(
            "Details:  not passed on (no helper at {})",
            paths.bridge_helper.display()
        );
    }
    if catalog::cache_path(&paths.cache_dir).exists() {
        println!("Games:    Discord's game list is downloaded");
    } else {
        println!(
            "Games:    Discord's game list is not downloaded yet (happens when a game starts)"
        );
    }

    let config_path = paths.config_path.display();
    match config {
        Ok(_) if !paths.config_path.exists() => {
            println!("Config:   none (optional; it would be {config_path})")
        }
        Ok(config) => println!(
            "Config:   {config_path} ({} game mapping(s), default client ID {})",
            config.games.len(),
            if config.default_client_id.is_some() {
                "set"
            } else {
                "not set"
            }
        ),
        Err(e) => println!("Config:   invalid: {e:#}"),
    }
    Ok(())
}

fn once() -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config_path)?;
    let game = daemon::detect_game(&paths, daemon::unix_ms());

    let activity = game.as_ref().map(|game| {
        // With no application to show the game under, print the payload an
        // application of your own would get.
        let resolved = config
            .resolve(game.app_id, game.official_id())
            .unwrap_or(Resolved {
                client_id: "",
                large_image: None,
                source: Source::Default,
            });
        daemon::build_activity(game, &resolved)
    });
    match &game {
        Some(game) => {
            println!("Detected game: {}", describe_game(game));
            println!("Discord client ID: {}", describe_client(&config, game));
        }
        None => println!("Detected game: none (the payload below clears the presence)"),
    }
    let payload = discord::set_activity_payload(std::process::id(), activity.as_ref(), "once");
    println!("Activity payload:");
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
