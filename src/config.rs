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
    // Never read through this field: the hand-written `Debug` below prints
    // `Redacted` in its place regardless of whether a token is set, and
    // `Config::from_toml` is the parser that will read it, in a later
    // commit than this one.
    #[allow(
        dead_code,
        reason = "read by the parser that resolves a Section into a Config"
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
// Nothing builds one of these yet: the parser that turns a `Section` into a
// `Config` is the next commit, not this one. Left in place, unconstructed,
// because it is this task's own declared interface.
#[allow(
    dead_code,
    reason = "constructed by the parser landing in a later commit"
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
// Neither exists yet, so nothing reads these constants until that commit.
#[allow(
    dead_code,
    reason = "read by PRINT_CONFIG and the parser, both landing in a later commit"
)]
pub const DEFAULT_FLUSH_MS: u64 = 1_000;
#[allow(
    dead_code,
    reason = "read by PRINT_CONFIG and the parser, both landing in a later commit"
)]
pub const DEFAULT_COALESCE_MS: u64 = 1_000;
#[allow(
    dead_code,
    reason = "read by PRINT_CONFIG and the parser, both landing in a later commit"
)]
pub const DEFAULT_BUFFER_LINES: usize = 2_000;
/// The floor `monitor_interval` is clamped up to, carried over from the old
/// 0.25 minute minimum. A monitor refreshing faster than this spends the
/// channel's whole rate budget redrawing embeds nobody asked for.
#[allow(
    dead_code,
    reason = "read by the parser that clamps monitor_interval, landing in a later commit"
)]
pub const MIN_MONITOR_INTERVAL_MS: u64 = 15_000;

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
}
