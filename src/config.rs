//! `~/.config/proton-presence/config.toml`.
//!
//! The file is optional. Without it every game Discord knows is shown
//! under Discord's own application for that game.
//!
//! ```toml
//! discord_game_list = true           # look games up in Discord's list
//! game_rich_presence = true          # pass on what the game itself reports
//! default_client_id = "123456789012345678"   # for games Discord lacks
//!
//! [games.1245620]                    # Steam app ID
//! client_id = "234567890123456789"   # use this application instead
//! large_image = "elden_ring"         # art asset key in that application
//! ```

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Whether to look games up in Discord's public game list.
    pub discord_game_list: bool,
    /// Whether to start the bridge helper next to Proton games so their
    /// own Rich Presence reaches Discord.
    pub game_rich_presence: bool,
    pub default_client_id: Option<String>,
    pub games: HashMap<u64, GameConfig>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            discord_game_list: true,
            game_rich_presence: true,
            default_client_id: None,
            games: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameConfig {
    pub client_id: Option<String>,
    pub large_image: Option<String>,
}

/// Where a game's client ID came from, in order of precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `client_id` under `[games.<id>]` in the config.
    Mapped,
    /// Discord's own application for the game, from its game list.
    Official,
    /// `default_client_id` in the config.
    Default,
}

/// The Discord application to use for one game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub client_id: &'a str,
    pub large_image: Option<&'a str>,
    pub source: Source,
}

/// Discord IDs are 64-bit integers; accept them quoted or bare.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Str(String),
    Int(u64),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    discord_game_list: Option<bool>,
    game_rich_presence: Option<bool>,
    default_client_id: Option<RawId>,
    #[serde(default)]
    games: HashMap<String, RawGame>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGame {
    client_id: Option<RawId>,
    large_image: Option<String>,
}

/// Normalises a client ID. An empty string means "not set", so the
/// installed template is valid before it is filled in.
fn client_id(raw: Option<RawId>, what: &str) -> Result<Option<String>> {
    let id = match raw {
        None => return Ok(None),
        Some(RawId::Int(n)) => n.to_string(),
        Some(RawId::Str(s)) => s.trim().to_string(),
    };
    if id.is_empty() {
        return Ok(None);
    }
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{what} must be a numeric Discord application ID, got {id:?}");
    }
    Ok(Some(id))
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text)?;
        let mut games = HashMap::new();
        for (key, game) in raw.games {
            let app_id: u64 = key
                .trim()
                .parse()
                .with_context(|| format!("[games.{key}]: {key:?} is not a Steam app ID"))?;
            games.insert(
                app_id,
                GameConfig {
                    client_id: client_id(game.client_id, &format!("games.{key}.client_id"))?,
                    large_image: game.large_image.filter(|s| !s.trim().is_empty()),
                },
            );
        }
        Ok(Config {
            discord_game_list: raw.discord_game_list.unwrap_or(true),
            game_rich_presence: raw.game_rich_presence.unwrap_or(true),
            default_client_id: client_id(raw.default_client_id, "default_client_id")?,
            games,
        })
    }

    /// Loads the file. A missing file is an empty config, not an error.
    pub fn load(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => Config::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// The Discord application for a game. `official` is the game's own
    /// application from Discord's game list, if it has one. Returns `None`
    /// when nothing applies and there is no default client ID.
    pub fn resolve<'a>(&'a self, app_id: u64, official: Option<&'a str>) -> Option<Resolved<'a>> {
        let game = self.games.get(&app_id);
        let large_image = game.and_then(|g| g.large_image.as_deref());
        let official = official.filter(|_| self.discord_game_list);

        if let Some(client_id) = game.and_then(|g| g.client_id.as_deref()) {
            return Some(Resolved {
                client_id,
                large_image,
                source: Source::Mapped,
            });
        }
        if let Some(client_id) = official {
            // Art asset keys belong to your own applications; Discord
            // shows the game's icon for its own application by itself.
            return Some(Resolved {
                client_id,
                large_image: None,
                source: Source::Official,
            });
        }
        Some(Resolved {
            client_id: self.default_client_id.as_deref()?,
            large_image,
            source: Source::Default,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const EXAMPLE: &str = r#"
default_client_id = "100"

[games.1245620]
client_id = "200"
large_image = "elden_ring"

[games.620]
large_image = "portal2"

[games.400]
client_id = 300
"#;

    #[test]
    fn parses_mappings_and_default() {
        let config = Config::parse(EXAMPLE).unwrap();
        assert_eq!(config.default_client_id.as_deref(), Some("100"));
        assert_eq!(config.games.len(), 3);
        assert_eq!(
            config.games[&1245620],
            GameConfig {
                client_id: Some("200".into()),
                large_image: Some("elden_ring".into()),
            }
        );
        assert_eq!(config.games[&400].client_id.as_deref(), Some("300"));
    }

    #[test]
    fn resolves_with_fallback_to_default() {
        let config = Config::parse(EXAMPLE).unwrap();

        let mapped = config.resolve(1245620, None).unwrap();
        assert_eq!(mapped.client_id, "200");
        assert_eq!(mapped.large_image, Some("elden_ring"));
        assert_eq!(mapped.source, Source::Mapped);

        let image_only = config.resolve(620, None).unwrap();
        assert_eq!(image_only.client_id, "100");
        assert_eq!(image_only.large_image, Some("portal2"));
        assert_eq!(image_only.source, Source::Default);

        let unmapped = config.resolve(999, None).unwrap();
        assert_eq!(unmapped.client_id, "100");
        assert_eq!(unmapped.large_image, None);
        assert_eq!(unmapped.source, Source::Default);
    }

    #[test]
    fn official_application_beats_default_but_not_a_mapping() {
        let config = Config::parse(EXAMPLE).unwrap();

        let official = config.resolve(999, Some("555")).unwrap();
        assert_eq!(official.client_id, "555");
        assert_eq!(official.source, Source::Official);

        // The image key belongs to the default application, not Discord's.
        let official = config.resolve(620, Some("555")).unwrap();
        assert_eq!(official.client_id, "555");
        assert_eq!(official.large_image, None);

        let mapped = config.resolve(1245620, Some("555")).unwrap();
        assert_eq!(mapped.client_id, "200");
        assert_eq!(mapped.source, Source::Mapped);
    }

    #[test]
    fn works_without_any_config() {
        let config = Config::default();
        assert!(config.discord_game_list);
        assert_eq!(config.resolve(5, None), None);
        let official = config.resolve(5, Some("555")).unwrap();
        assert_eq!(
            (official.client_id, official.source),
            ("555", Source::Official)
        );
    }

    #[test]
    fn game_list_can_be_switched_off() {
        let config = Config::parse("discord_game_list = false\n").unwrap();
        assert!(!config.discord_game_list);
        assert_eq!(config.resolve(5, Some("555")), None);

        let config =
            Config::parse("discord_game_list = false\ndefault_client_id = \"100\"\n").unwrap();
        assert_eq!(config.resolve(5, Some("555")).unwrap().client_id, "100");
        assert!(Config::parse("discord_game_list = \"yes\"").is_err());
    }

    #[test]
    fn game_rich_presence_can_be_switched_off() {
        assert!(Config::default().game_rich_presence);
        assert!(Config::parse("").unwrap().game_rich_presence);
        let config = Config::parse("game_rich_presence = false\n").unwrap();
        assert!(!config.game_rich_presence);
        assert!(config.discord_game_list);
    }

    #[test]
    fn nothing_resolves_without_a_default() {
        let config = Config::parse("[games.5]\nclient_id = \"7\"\n").unwrap();
        assert_eq!(config.resolve(5, None).unwrap().client_id, "7");
        assert_eq!(config.resolve(6, None), None);
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::default().resolve(5, None), None);
    }

    #[test]
    fn empty_client_id_counts_as_unset() {
        let config = Config::parse("default_client_id = \"\"\n").unwrap();
        assert_eq!(config.default_client_id, None);
    }

    #[test]
    fn rejects_invalid_configs() {
        assert!(Config::parse("default_client_id = \"abc\"").is_err());
        assert!(Config::parse("[games.eldenring]\nclient_id = \"1\"").is_err());
        assert!(Config::parse("[games.5]\nclient_id = \"x1\"").is_err());
        assert!(Config::parse("[games.5]\nlarge_imgae = \"typo\"").is_err());
        assert!(Config::parse("defualt_client_id = \"1\"").is_err());
        assert!(Config::parse("default_client_id = ").is_err());
    }

    #[test]
    fn missing_file_is_an_empty_config() {
        let tmp = TempDir::new("config");
        let path = tmp.path().join("config.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());

        fs::write(&path, EXAMPLE).unwrap();
        assert_eq!(Config::load(&path).unwrap().games.len(), 3);

        fs::write(&path, "default_client_id = [").unwrap();
        let err = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(err.contains("config.toml"), "{err}");
    }
}
