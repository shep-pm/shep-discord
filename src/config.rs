//! The `[discord]` section of `dogs.toml`.
//!
//! The daemon serves this per request rather than caching it, and it never
//! travels through the environment: an environment variable is readable
//! from the process table, inherited by every child, and captured into a
//! crash dump. A bot token is a bearer credential, so that matters here
//! more than it does for most dogs.

use core::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use shep_client::{dogs::DogConfig, shep_core::values::UpDuration};

use crate::error::Error;

/// What an operator may write under `[discord]`. Every field is optional so
/// a half-filled section still parses and the error names the missing key,
/// rather than serde refusing the whole table.
#[derive(Default, Deserialize, JsonSchema, DogConfig)]
#[serde(deny_unknown_fields)]
#[schemars(title = "discord", description = "Settings for the shep-discord dog.")]
pub struct Section {
    /// The bot token. Marked secret so lookout's config pane masks it and
    /// the published schema says what it is.
    #[shep(secret)]
    #[schemars(description = "Discord bot token.")]
    // Never read through this field for logging: the hand-written `Debug`
    // below prints `Redacted` in its place regardless of whether a token is
    // set. `Config::from_toml` is the one reader that legitimately reaches
    // it, to move it into `Config::token`, but nothing in `main` calls
    // `from_toml` yet; that wiring lands once this dog reads `dogs.toml`
    // over the socket, in a later task.
    #[allow(
        dead_code,
        reason = "read by Config::from_toml, unreached from main until a later task wires it in"
    )]
    pub token: Option<String>,
    #[schemars(description = "The guild (server) this bot serves.")]
    pub guild_id: Option<u64>,
    #[schemars(description = "Channel for the live monitor. Unset disables it.")]
    pub monitor_channel: Option<u64>,
    // `with` names shep-core's own grammar while the field stays a
    // `String`, because an operator writes "1m" and lookout needs the
    // `$ref` name to tell a duration from a byte size. This is the shape
    // shep-log-rotate's `Section` uses for exactly this reason.
    #[schemars(
        with = "Option<UpDuration>",
        description = "How often the monitor refreshes, e.g. \"1m\". Unset means the monitor does not run from boot."
    )]
    pub monitor_interval: Option<String>,
    #[schemars(description = "Channel for stdout lines. Unset disables that stream.")]
    pub log_channel: Option<u64>,
    #[schemars(description = "Channel for stderr lines. Unset disables that stream.")]
    pub err_channel: Option<u64>,
    #[schemars(
        with = "Option<UpDuration>",
        description = "How often the buffer drains, e.g. \"1s\"."
    )]
    pub flush: Option<String>,
    #[schemars(
        with = "Option<UpDuration>",
        description = "How wide a window joins lines into one embed, e.g. \"1s\"."
    )]
    pub coalesce: Option<String>,
    #[schemars(description = "How many lines the buffer holds before dropping oldest.")]
    pub buffer_lines: Option<usize>,
    #[schemars(description = "Hide other dogs from listings and the monitor.")]
    pub ignore_dogs: Option<bool>,
}

/// Prints as `<redacted>`, with no surrounding quotes.
///
/// A plain `&"<redacted>"` renders through `str`'s own `Debug`, which quotes
/// it, and a reader skimming an error chain for a real token cannot tell a
/// quoted placeholder from a quoted secret at a glance. This type's `Debug`
/// writes the words directly instead, so the placeholder cannot be mistaken
/// for the value it stands in for.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted>")
    }
}

impl fmt::Debug for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Section")
            .field("token", &Redacted)
            .field("guild_id", &self.guild_id)
            .field("monitor_channel", &self.monitor_channel)
            .field("monitor_interval", &self.monitor_interval)
            .field("log_channel", &self.log_channel)
            .field("err_channel", &self.err_channel)
            .field("flush", &self.flush)
            .field("coalesce", &self.coalesce)
            .field("buffer_lines", &self.buffer_lines)
            .field("ignore_dogs", &self.ignore_dogs)
            .finish()
    }
}

/// The resolved settings, with every optional field decided.
// `Config::from_toml` builds one of these, but nothing in `main` calls
// `from_toml` yet: reading `dogs.toml` over the socket is a later task's
// job. Only the tests in this file construct one for now.
#[allow(
    dead_code,
    reason = "constructed by Config::from_toml, unreached from main until a later task wires it in"
)]
pub struct Config {
    pub token: String,
    pub guild_id: u64,
    pub monitor_channel: Option<u64>,
    pub monitor_interval: Option<UpDuration>,
    pub log_channel: Option<u64>,
    pub err_channel: Option<u64>,
    pub flush: UpDuration,
    pub coalesce: UpDuration,
    pub buffer_lines: usize,
    pub ignore_dogs: bool,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("token", &Redacted)
            .field("guild_id", &self.guild_id)
            .field("monitor_channel", &self.monitor_channel)
            .field("monitor_interval", &self.monitor_interval)
            .field("log_channel", &self.log_channel)
            .field("err_channel", &self.err_channel)
            .field("flush", &self.flush)
            .field("coalesce", &self.coalesce)
            .field("buffer_lines", &self.buffer_lines)
            .field("ignore_dogs", &self.ignore_dogs)
            .finish()
    }
}

/// Defaults, named once so `PRINT_CONFIG` and the parser cannot disagree.
// Read by `Config::from_toml`, which is itself unreached from `main` until
// a later task wires it in; see the note on `Config` above.
#[allow(
    dead_code,
    reason = "read by Config::from_toml, unreached from main until a later task wires it in"
)]
pub const DEFAULT_FLUSH_MS: u64 = 1_000;
#[allow(
    dead_code,
    reason = "read by Config::from_toml, unreached from main until a later task wires it in"
)]
pub const DEFAULT_COALESCE_MS: u64 = 1_000;
#[allow(
    dead_code,
    reason = "read by Config::from_toml, unreached from main until a later task wires it in"
)]
pub const DEFAULT_BUFFER_LINES: usize = 2_000;
/// The floor `monitor_interval` is clamped up to, carried over from the old
/// 0.25 minute minimum. A monitor refreshing faster than this spends the
/// channel's whole rate budget redrawing embeds nobody asked for.
#[allow(
    dead_code,
    reason = "read by Config::from_toml, unreached from main until a later task wires it in"
)]
pub const MIN_MONITOR_INTERVAL_MS: u64 = 15_000;

/// Parse `value` into an [`UpDuration`], naming `field` in the [`Error`] so
/// an operator can find the offending key without reading this dog's
/// source.
#[allow(
    dead_code,
    reason = "called by Config::from_toml, unreached from main until a later task wires it in"
)]
fn parse_duration(value: String, field: &'static str) -> Result<UpDuration, Error> {
    value.parse::<UpDuration>().map_err(|source| {
        Error::Config(format!(
            "{field} = \"{value}\" is not a duration shep accepts: {source}"
        ))
    })
}

impl Config {
    /// Parse the `[discord]` table's body into a resolved [`Config`].
    ///
    /// An empty string is not refused by itself: it parses to a [`Section`]
    /// with every field `None`, and resolving that fails on the first field
    /// this dog cannot default, `token`. That is byte for byte the section
    /// the daemon serves for a name nobody adopted, so it is also the
    /// likeliest error an operator sees, and the message names the missing
    /// key rather than saying "invalid config".
    ///
    /// # Errors
    /// [`Error::Config`] when the text is not valid TOML, carries a key
    /// this dog does not know, is missing `token` or `guild_id`, or gives
    /// `flush`, `coalesce` or `monitor_interval` a value [`UpDuration`]
    /// does not accept.
    // Unreached from `main` until a later task reads `dogs.toml` over the
    // socket and calls this; only the tests below call it for now.
    #[allow(
        dead_code,
        reason = "called once main() reads dogs.toml over the socket, in a later task"
    )]
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        let section: Section =
            toml::from_str(text).map_err(|err| Error::Config(err.to_string()))?;

        let token = section
            .token
            .ok_or_else(|| Error::Config("token is required".to_owned()))?;
        let guild_id = section
            .guild_id
            .ok_or_else(|| Error::Config("guild_id is required".to_owned()))?;

        let flush = section
            .flush
            .map(|value| parse_duration(value, "flush"))
            .transpose()?
            .unwrap_or(UpDuration::from_millis(DEFAULT_FLUSH_MS));
        let coalesce = section
            .coalesce
            .map(|value| parse_duration(value, "coalesce"))
            .transpose()?
            .unwrap_or(UpDuration::from_millis(DEFAULT_COALESCE_MS));
        // Raised to the floor rather than refused: an operator who wrote
        // "1s" wanted a live monitor, not a rejected config.
        let floor = UpDuration::from_millis(MIN_MONITOR_INTERVAL_MS);
        let monitor_interval = section
            .monitor_interval
            .map(|value| parse_duration(value, "monitor_interval"))
            .transpose()?
            .map(|interval| interval.max(floor));

        Ok(Self {
            token,
            guild_id,
            monitor_channel: section.monitor_channel,
            monitor_interval,
            log_channel: section.log_channel,
            err_channel: section.err_channel,
            flush,
            coalesce,
            buffer_lines: section.buffer_lines.unwrap_or(DEFAULT_BUFFER_LINES),
            ignore_dogs: section.ignore_dogs.unwrap_or(false),
        })
    }
}

/// A commented block naming every `[discord]` option and its default, for
/// `shep-discord --print-config`.
///
/// Every line is commented, so appending it to `dogs.toml` changes nothing
/// until the operator uncomments a line. `token` and `guild_id` have no
/// real default; the value shown there is a placeholder to overwrite, not a
/// setting the dog would otherwise pick.
///
/// The header is the bare name, and the file is `dogs.toml`. It is never
/// `[dog.discord]` in `shep.toml`: a shepherd that has migrated a section
/// there and finds the same dog named again in `dogs.toml` refuses to boot
/// rather than guess which file the operator meant.
pub const PRINT_CONFIG: &str = r#"[discord]
# Discord bot token. Required: shep-discord will not run without one.
#token = "your-bot-token"
# The guild (server) this bot serves. Required.
#guild_id = 123456789012345678
# Channel for the live monitor. Unset disables it.
#monitor_channel = 123456789012345678
# How often the monitor refreshes, e.g. "1m". Unset means the monitor does
# not run from boot. Below 15s is raised to it: a monitor refreshing that
# often spends the channel's whole rate budget redrawing embeds nobody
# asked for.
#monitor_interval = "1m"
# Channel for stdout lines. Unset disables that stream.
#log_channel = 123456789012345678
# Channel for stderr lines. Unset disables that stream.
#err_channel = 123456789012345678
# How often the buffer drains, e.g. "1s".
#flush = "1s"
# How wide a window joins lines into one embed, e.g. "1s".
#coalesce = "1s"
# How many lines the buffer holds before dropping the oldest.
#buffer_lines = 2000
# Hide other dogs from listings and the monitor.
#ignore_dogs = false
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A derived `Debug` on a type holding a bot token puts that token into
    /// every log line, panic message and error chain that prints it. An
    /// exact string, not a `contains`: a redaction that stops covering a
    /// newly added field still passes a `contains` check.
    #[test]
    fn the_token_never_reaches_a_debug_line() {
        let section = Section {
            token: Some("MTIzNDU2Nzg5.GaBcDe.ThisIsNotARealToken".to_owned()),
            guild_id: Some(42),
            ..Section::default()
        };
        let rendered = format!("{section:?}");
        assert_eq!(
            rendered,
            "Section { token: <redacted>, guild_id: Some(42), monitor_channel: None, \
             monitor_interval: None, log_channel: None, err_channel: None, flush: None, \
             coalesce: None, buffer_lines: None, ignore_dogs: None }"
        );
        assert!(!rendered.contains("ThisIsNotARealToken"), "{rendered}");
    }

    #[test]
    fn a_section_without_a_token_names_the_missing_key() {
        let err = Config::from_toml("guild_id = 1")
            .expect_err("refused")
            .to_string();
        assert!(err.contains("token"), "{err}");
    }

    #[test]
    fn an_empty_section_still_names_what_is_missing() {
        // The daemon answers `DogConfig` for a name nobody adopted with an
        // empty section, byte for byte what a dog on its defaults gets. So
        // this is the likeliest error an operator sees, and it has to be the
        // clearest one.
        let err = Config::from_toml("").expect_err("refused").to_string();
        assert!(err.contains("token"), "{err}");
    }

    #[test]
    fn a_monitor_interval_under_the_floor_is_raised_to_it() {
        let config = Config::from_toml("token = \"t\"\nguild_id = 1\nmonitor_interval = \"1s\"\n")
            .expect("parsed");
        assert_eq!(
            config.monitor_interval.expect("set").as_millis(),
            MIN_MONITOR_INTERVAL_MS,
            "a monitor refreshing every second spends the channel's whole rate budget"
        );
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let err = Config::from_toml("token = \"t\"\nguild_id = 1\nbuffer_line = 10\n")
            .expect_err("refused")
            .to_string();
        assert!(err.contains("buffer_line"), "{err}");
    }

    #[test]
    fn the_defaults_are_what_the_printed_block_says() {
        let config = Config::from_toml("token = \"t\"\nguild_id = 1\n").expect("parsed");
        assert_eq!(config.flush.as_millis(), DEFAULT_FLUSH_MS);
        assert_eq!(config.coalesce.as_millis(), DEFAULT_COALESCE_MS);
        assert_eq!(config.buffer_lines, DEFAULT_BUFFER_LINES);
        assert!(!config.ignore_dogs);
    }

    /// The schema and the printed block are the second and third places that
    /// have to agree with `Section`'s fields about what a setting is. This is
    /// the edge between them.
    #[test]
    fn the_schema_and_the_printed_block_name_the_same_settings() {
        let schema = shep_client::dogs::config_schema::<Section>().expect("schema");
        let value = serde_json::to_value(&schema).expect("json");
        let properties = value["properties"].as_object().expect("properties");
        for key in properties.keys() {
            assert!(
                PRINT_CONFIG.contains(&format!("{key} ="))
                    || PRINT_CONFIG.contains(&format!("# {key} =")),
                "PRINT_CONFIG omits {key}"
            );
        }
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(PRINT_CONFIG);
    }
}
