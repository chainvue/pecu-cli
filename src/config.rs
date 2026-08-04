//! Where things live on disk, and which chain we are pointed at.
//!
//! Settings resolve in one order, everywhere: **command-line flag → environment
//! → config file → built-in default**. Nothing else reads the environment, so
//! that order is a property of this module rather than a convention every
//! command has to remember.
//!
//! The config file is optional. A fresh install has no file at all and still
//! works, because the two profiles that matter are built in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cli::Globals;

/// The chain we point at unless told otherwise. Testnet, deliberately: this is
/// an example app, and the first thing a reader runs should not spend real money.
pub const DEFAULT_PROFILE: &str = "testnet";

#[derive(Debug, Error, Diagnostic)]
pub enum ConfigError {
    #[error("cannot work out where to put pecu's files")]
    #[diagnostic(
        code(pecu::no_home),
        help("set $PECU_HOME to a directory pecu may write to")
    )]
    NoHome,

    #[error("cannot read {}", path.display())]
    #[diagnostic(code(pecu::config_unreadable))]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{} is not valid config", path.display())]
    #[diagnostic(
        code(pecu::config_invalid),
        help("delete the file to fall back to the built-in profiles")
    )]
    Invalid {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("no profile named `{name}`")]
    #[diagnostic(code(pecu::unknown_profile), help("known profiles: {known}"))]
    UnknownProfile { name: String, known: String },
}

/// Every path pecu writes to, rooted at one directory.
///
/// `$PECU_HOME` overrides everything, which is what makes the tests hermetic —
/// they point it at a temporary directory and cannot touch a real keystore.
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Rooted at an explicit directory, so the unit tests never depend on
    /// process-wide environment state. Integration tests set `$PECU_HOME`.
    #[cfg(test)]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn resolve() -> Result<Self, ConfigError> {
        if let Some(forced) = std::env::var_os("PECU_HOME") {
            return Ok(Self {
                root: PathBuf::from(forced),
            });
        }
        // XDG rather than the platform convention on purpose: `~/.config` is
        // where someone reaching for a terminal wallet will look, on any OS.
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or(ConfigError::NoHome)?;
        Ok(Self {
            root: base.join("verus-pecu"),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// One encrypted file per key.
    pub fn keys_dir(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// How many keys the keystore holds. A missing directory is zero, not an
    /// error: not having made a key yet is the normal first state.
    pub fn key_count(&self) -> usize {
        std::fs::read_dir(self.keys_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                    .count()
            })
            .unwrap_or(0)
    }
}

/// The config file's shape. Every field is optional; the file itself is optional.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Which profile to use when `--profile` is not given.
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileFile>,
}

/// One profile, as written in the file. Anything left out falls back to the
/// built-in profile of the same name, then to testnet's values.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileFile {
    pub node: Option<String>,
    pub explorer: Option<String>,
    pub currency: Option<String>,
    /// Whether commands that move money may run against this profile.
    pub allow_spend: Option<bool>,
}

/// A profile with every field decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub node: String,
    pub explorer: String,
    /// The chain's native currency, for labelling amounts.
    pub currency: String,
    /// Mainnet ships as `false`: spending real coins from an example app should
    /// take a deliberate act, not a forgotten `--profile`.
    pub allow_spend: bool,
}

impl Profile {
    fn builtin(name: &str) -> Option<Self> {
        match name {
            "testnet" => Some(Self {
                name: name.into(),
                node: "https://api.verustest.net".into(),
                explorer: "https://testex.verus.io".into(),
                currency: "VRSCTEST".into(),
                allow_spend: true,
            }),
            "mainnet" => Some(Self {
                name: name.into(),
                node: "https://api.verus.services".into(),
                explorer: "https://explorer.verus.io".into(),
                currency: "VRSC".into(),
                allow_spend: false,
            }),
            _ => None,
        }
    }

    fn builtin_names() -> &'static [&'static str] {
        &["testnet", "mainnet"]
    }
}

/// Everything a command needs to know about its surroundings.
#[derive(Debug, Clone)]
pub struct Settings {
    pub paths: Paths,
    pub profile: Profile,
    /// Whether a config file was found. Reported by `doctor`, because "my
    /// setting is being ignored" is nearly always this.
    pub config_exists: bool,
}

impl Settings {
    /// Apply the resolution order to the flags we were given.
    ///
    /// `Globals::node` and `Globals::profile` already carry their environment
    /// variables — clap resolves those — so the remaining work is the file and
    /// the built-ins.
    pub fn resolve(globals: &Globals) -> Result<Self, ConfigError> {
        Self::resolve_in(
            Paths::resolve()?,
            globals.profile.as_deref(),
            globals.node.as_deref(),
        )
    }

    /// The same, with the environment already read for us. Everything the
    /// resolution order does happens here; [`Settings::resolve`] only supplies
    /// the inputs.
    pub fn resolve_in(
        paths: Paths,
        profile_flag: Option<&str>,
        node_flag: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let path = paths.config_file();
        let (file, config_exists) = match std::fs::read_to_string(&path) {
            Ok(text) => (
                toml::from_str::<ConfigFile>(&text).map_err(|source| ConfigError::Invalid {
                    path: path.clone(),
                    source,
                })?,
                true,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (ConfigFile::default(), false)
            }
            Err(source) => return Err(ConfigError::Unreadable { path, source }),
        };

        let name = profile_flag
            .map(str::to_string)
            .or_else(|| file.default_profile.clone())
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string());

        let overrides = file.profiles.get(&name).cloned();
        let mut profile = match (Profile::builtin(&name), &overrides) {
            (Some(builtin), _) => builtin,
            // A profile that exists only in the file still needs defaults for
            // whatever it leaves out; testnet's are the safe ones to borrow.
            (None, Some(_)) => Profile {
                name: name.clone(),
                ..Profile::builtin(DEFAULT_PROFILE).expect("testnet is built in")
            },
            (None, None) => {
                let mut known: Vec<String> = Profile::builtin_names()
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect();
                known.extend(file.profiles.keys().cloned());
                known.sort();
                known.dedup();
                return Err(ConfigError::UnknownProfile {
                    name,
                    known: known.join(", "),
                });
            }
        };

        if let Some(overrides) = overrides {
            if let Some(node) = overrides.node {
                profile.node = node;
            }
            if let Some(explorer) = overrides.explorer {
                profile.explorer = explorer;
            }
            if let Some(currency) = overrides.currency {
                profile.currency = currency;
            }
            if let Some(allow_spend) = overrides.allow_spend {
                profile.allow_spend = allow_spend;
            }
        }

        // The flag wins over everything, including a profile that named a node.
        if let Some(node) = node_flag {
            profile.node = node.to_string();
        }

        Ok(Self {
            paths,
            profile,
            config_exists,
        })
    }
}

/// Render a path with `$HOME` collapsed back to `~`, which is how anyone reading
/// it thinks of it.
pub fn tildify(path: &Path) -> String {
    let display = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && display.starts_with(&home) => {
            format!("~{}", &display[home.len()..])
        }
        _ => display,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config directory with `contents` written into `config.toml`, or none.
    fn settings(
        contents: Option<&str>,
        profile: Option<&str>,
        node: Option<&str>,
    ) -> Result<Settings, ConfigError> {
        let dir = tempfile::tempdir().expect("a temp dir");
        if let Some(contents) = contents {
            std::fs::write(dir.path().join("config.toml"), contents).expect("writable temp dir");
        }
        // `resolve_in` reads the file eagerly, so the directory may go away with
        // the guard at the end of this call.
        Settings::resolve_in(Paths::at(dir.path()), profile, node)
    }

    #[test]
    fn with_no_config_file_the_built_in_testnet_profile_applies() {
        let settings = settings(None, None, None).unwrap();
        assert!(!settings.config_exists);
        assert_eq!(settings.profile.name, "testnet");
        assert_eq!(settings.profile.node, "https://api.verustest.net");
        assert_eq!(settings.profile.currency, "VRSCTEST");
        assert!(settings.profile.allow_spend);
    }

    #[test]
    fn mainnet_ships_unable_to_spend() {
        let settings = settings(None, Some("mainnet"), None).unwrap();
        assert_eq!(settings.profile.currency, "VRSC");
        assert!(
            !settings.profile.allow_spend,
            "mainnet must require an explicit opt-in before it can spend"
        );
    }

    #[test]
    fn the_file_can_choose_the_default_profile() {
        let settings = settings(Some("default_profile = \"mainnet\"\n"), None, None).unwrap();
        assert_eq!(settings.profile.name, "mainnet");
        assert!(settings.config_exists);
    }

    #[test]
    fn the_flag_beats_the_files_default_profile() {
        let settings = settings(
            Some("default_profile = \"mainnet\"\n"),
            Some("testnet"),
            None,
        )
        .unwrap();
        assert_eq!(settings.profile.name, "testnet");
    }

    #[test]
    fn file_overrides_merge_onto_the_built_in_profile() {
        let settings = settings(
            Some("[profiles.testnet]\nnode = \"https://node.example\"\n"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(settings.profile.node, "https://node.example");
        // Untouched fields still come from the built-in.
        assert_eq!(settings.profile.explorer, "https://testex.verus.io");
        assert_eq!(settings.profile.currency, "VRSCTEST");
    }

    #[test]
    fn the_node_flag_beats_everything() {
        let settings = settings(
            Some("[profiles.testnet]\nnode = \"https://node.example\"\n"),
            None,
            Some("https://flag.example"),
        )
        .unwrap();
        assert_eq!(settings.profile.node, "https://flag.example");
    }

    #[test]
    fn a_profile_that_exists_only_in_the_file_inherits_testnets_defaults() {
        let settings = settings(
            Some("[profiles.mine]\nnode = \"https://mine.example\"\n"),
            Some("mine"),
            None,
        )
        .unwrap();
        assert_eq!(settings.profile.name, "mine");
        assert_eq!(settings.profile.node, "https://mine.example");
        assert_eq!(settings.profile.currency, "VRSCTEST");
    }

    #[test]
    fn an_unknown_profile_lists_the_ones_that_exist() {
        let error = settings(Some("[profiles.mine]\n"), Some("typo"), None).unwrap_err();
        let ConfigError::UnknownProfile { name, known } = error else {
            panic!("expected UnknownProfile, got {error:?}");
        };
        assert_eq!(name, "typo");
        assert_eq!(known, "mainnet, mine, testnet");
    }

    #[test]
    fn a_broken_config_file_is_an_error_rather_than_a_silent_default() {
        let error = settings(Some("this is not toml"), None, None).unwrap_err();
        assert!(matches!(error, ConfigError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn a_misspelled_key_is_refused_instead_of_ignored() {
        // Without deny_unknown_fields this would parse, and the setting would
        // silently do nothing.
        let error = settings(
            Some("[profiles.testnet]\nnodes = \"https://node.example\"\n"),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn key_count_treats_a_missing_keystore_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Paths::at(dir.path()).key_count(), 0);
    }
}
