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
    // it, to move it into `Config::token`.
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
///
/// `PartialEq` is derived, not hand-written, and it does compare `token`:
/// it exists for the round-trip test below to compare a whole `Config`
/// against another in one assertion, never to authenticate anything, so
/// there is nothing here for a timing-safe comparison to protect.
#[derive(PartialEq)]
pub struct Config {
    // Read through this field by the derived `PartialEq::eq` above, which
    // is why this needs no `#[allow(dead_code)]`: the generated `eq`
    // counts as a read for dead-code purposes whether or not anything
    // calls it in a plain build, and nothing here does. The hand-written `Debug` below still redacts it rather than
    // reading it, which is a decision about what a log line should show,
    // not evidence this field goes otherwise unread.
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
pub const DEFAULT_FLUSH_MS: u64 = 1_000;
pub const DEFAULT_COALESCE_MS: u64 = 1_000;
pub const DEFAULT_BUFFER_LINES: usize = 2_000;
/// The floor `monitor_interval` is clamped up to, carried over from the old
/// 0.25 minute minimum. A monitor refreshing faster than this spends the
/// channel's whole rate budget redrawing embeds nobody asked for.
pub const MIN_MONITOR_INTERVAL_MS: u64 = 15_000;

/// Parse `value` into an [`UpDuration`], naming `field` in the [`Error`] so
/// an operator can find the offending key without reading this dog's
/// source.
fn parse_duration(value: &str, field: &'static str) -> Result<UpDuration, Error> {
    value.parse::<UpDuration>().map_err(|source| {
        Error::Config(format!("{field} is not a duration shep accepts: {source}"))
    })
}

/// Turn a TOML parse failure into an [`Error`] carrying the line number
/// and nothing else.
///
/// The line number is the whole of it on purpose. It is an integer
/// counted from a byte offset, so it cannot carry a secret, and every
/// other part of what `toml` produces can.
///
/// `toml::de::Error`'s `Display` renders the offending source line under
/// a caret, and that line is very often `token = "..."`. Its
/// `message()` is no safer: `invalid type: string "MTIz.SECRET.abc",
/// expected u64` is what an operator gets for pasting a bot token onto
/// `guild_id`, and every field in [`Section`] except `token` itself is a
/// `u64`, `usize` or `bool`, so all of them quote a pasted string back.
/// `invalid type: integer `9912345678`, expected a string` is the same
/// thing for a token written unquoted.
///
/// From there it goes to stderr, to `$SHEP_HOME/logs/<name>-0-err.log`
/// through the shepherd's log pump, and back out through `shep bleats`.
/// Everything else in this module guards that value: `Section` and
/// `Config` have hand-written `Debug` implementations printing
/// `<redacted>`, pinned by an exact-string test, and the module doc
/// explains that the token never travels through the environment for the
/// same reason.
///
/// This landed on the line number only after two narrower rules, and the
/// way they failed is the argument. First the rendered source was
/// dropped and `message()` kept, which `invalid type: integer` walked
/// straight past. Then `message()` was dropped for a failing line that
/// assigned `token`, which a token pasted onto `guild_id` walked past
/// just as easily, because the line it sits on is not the token's line.
/// Both were attempts to enumerate the messages that can quote a value,
/// and that is not a set anybody enumerates correctly on the third try
/// either.
///
/// shep reached the same place in its own dog framework.
/// `shep-cli/src/dog/mod.rs` documents `DogRunError::Section`'s
/// `message` as "the parser's full complaint, which can quote the
/// offending line", hand-writes a `Debug` that redacts it because it
/// "can quote a `[dog.<name>]` webhook URL verbatim", and has `run_bark`
/// discard the error outright: "The fact, not the value: a `[bark]`
/// section can carry a webhook URL with a bearer token in its path."
/// This says one thing more than bark does, which is which line.
///
/// One narrower rule was proposed and measured, and is written down
/// here because it is a reasonable thing to want and should not be
/// rediscovered from scratch: drop `message()` only when it starts with
/// `invalid type:`, which is the one shape found to quote a value.
/// `unknown field`, `duplicate key` and `expected newline` all quote a
/// key or nothing, so `unknown field `buffer_line`` would survive and
/// the commonest diagnosis would be kept.
///
/// It holds for every message this `Section` can currently produce. It
/// was not taken for two reasons. The prefix is `serde`'s default
/// `Display` wording rather than a documented contract, so it is
/// something an upstream bump can change. And it stays correct only
/// while every field here is a scalar: give one an enum and `unknown
/// variant `SECRET`` quotes a value without saying `invalid type`. The
/// leak test below would catch the first of those loudly and the second
/// only if somebody thought to add a row. Taking a smaller error over a
/// rule that needs both of those to keep holding is a judgement call,
/// not a fact, and this is the conservative side of it.
///
/// What it costs is real and worth naming: `unknown field
/// `buffer_line`` was the whole diagnosis for the commonest mistake in
/// this file, and it is gone. An operator gets the line number, opens
/// `dogs.toml` at it, and compares against `--print-config` or the
/// README table. That is a worse error and a safe one, and a bearer
/// credential is not the thing to spend on a better error message.
fn parse_failure(text: &str, err: &toml::de::Error) -> Error {
    let offset = err.span().map_or(0, |span| span.start).min(text.len());
    let line_number = text[..offset].matches('\n').count() + 1;
    Error::Config(format!(
        "line {line_number} does not parse. This dog will not print what is on it or what the \
         parser made of it, because a mistyped section is exactly where a bot token ends up in \
         the wrong place. Compare that line against shep-discord --print-config."
    ))
}

/// Refuse a present `0`, naming `field` in the [`Error`].
///
/// A Discord snowflake is never `0`, so a `0` here is a placeholder an
/// operator forgot to replace or a typo, not a value that could ever name a
/// real guild or channel. Refusing it at parse time surfaces that mistake
/// immediately, rather than as commands registered against a guild that
/// does not exist or a send that fails on every flush. `None` passes
/// through unchanged: an unset channel is legitimate, and only a `0`
/// somebody actually wrote is refused.
fn refuse_zero(value: Option<u64>, field: &'static str) -> Result<Option<u64>, Error> {
    match value {
        Some(0) => Err(Error::Config(format!("{field} must not be 0"))),
        other => Ok(other),
    }
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
    /// this dog does not know, gives `flush`, `coalesce` or
    /// `monitor_interval` a value [`UpDuration`] does not accept, gives
    /// `buffer_lines` a `0`, or gives `guild_id`, `monitor_channel`,
    /// `log_channel` or `err_channel` a present `0`: a Discord snowflake
    /// is never `0`.
    ///
    /// [`Error::Unconfigured`] when `token` or `guild_id` is absent,
    /// which is a section nobody has filled in rather than one anybody
    /// has got wrong. Which of the two comes back decides whether
    /// `crate::run` stays up or stops, so a new check added here belongs
    /// in whichever of these two paragraphs describes it.
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        let section: Section = toml::from_str(text).map_err(|err| parse_failure(text, &err))?;

        // Every value the operator actually wrote is checked first, and
        // the two keys that may simply be absent are checked last. The
        // order is the whole difference between the two variants meaning
        // something and meaning nothing.
        //
        // Writing a value is a deliberate act, and getting it wrong is a
        // mistake this dog should stop for. Leaving a key out may be
        // nothing more than not having typed it yet, which is every
        // freshly adopted dog. So a section that is both, `guild_id = 0`
        // with no `token`, is wrong rather than unfinished: fixing only
        // the `token` would not make it run, and reporting the absent key
        // first would leave the dog online saying a value it had already
        // refused was fine.
        //
        // Checked the other way round until CodeRabbit caught it on
        // shep-pm/shep-discord#3. `token` was resolved before anything
        // else, so `guild_id = 0` alone, or `token` set with
        // `buffer_lines = 0`, answered `Unconfigured` and the run loop
        // stayed up on a value it exists to exit for.
        //
        // An empty section is unaffected, which is the case that matters
        // most: nothing is written, so nothing below can fail, and the
        // first thing to answer is still `token is required`.
        let guild_id = refuse_zero(section.guild_id, "guild_id")?;
        let monitor_channel = refuse_zero(section.monitor_channel, "monitor_channel")?;
        let log_channel = refuse_zero(section.log_channel, "log_channel")?;
        let err_channel = refuse_zero(section.err_channel, "err_channel")?;

        let flush = section
            .flush
            .map(|value| parse_duration(&value, "flush"))
            .transpose()?
            .unwrap_or(UpDuration::from_millis(DEFAULT_FLUSH_MS));
        let coalesce = section
            .coalesce
            .map(|value| parse_duration(&value, "coalesce"))
            .transpose()?
            .unwrap_or(UpDuration::from_millis(DEFAULT_COALESCE_MS));
        // Raised to the floor rather than refused: an operator who wrote
        // "1s" wanted a live monitor, not a rejected config.
        let floor = UpDuration::from_millis(MIN_MONITOR_INTERVAL_MS);
        let monitor_interval = section
            .monitor_interval
            .map(|value| parse_duration(&value, "monitor_interval"))
            .transpose()?
            .map(|interval| interval.max(floor));

        let buffer_lines = section.buffer_lines.unwrap_or(DEFAULT_BUFFER_LINES);
        if buffer_lines == 0 {
            return Err(Error::Config("buffer_lines must be at least 1".to_owned()));
        }

        // Last, and only once nothing written is wrong. `refuse_zero`
        // above has already answered for a written `0`, so these two
        // report only a key nobody has typed.
        let token = section
            .token
            .ok_or_else(|| Error::Unconfigured("token is required".to_owned()))?;
        let guild_id =
            guild_id.ok_or_else(|| Error::Unconfigured("guild_id is required".to_owned()))?;

        Ok(Self {
            token,
            guild_id,
            monitor_channel,
            monitor_interval,
            log_channel,
            err_channel,
            flush,
            coalesce,
            buffer_lines,
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

    /// A `dogs.toml` that does not parse must not carry the bot token out
    /// in the error, which is what `toml::de::Error`'s own `Display` does:
    /// it quotes the offending source line under a caret, and that line is
    /// usually the one the token is written on. The error reaches stderr,
    /// `$SHEP_HOME/logs/<name>-0-err.log` through the shepherd's log pump,
    /// and `shep bleats` after that, so this is the token in clear text in
    /// a file an operator greps.
    ///
    /// Every other guard in this module was already in place. `Section`
    /// and `Config` print `<redacted>`, pinned by an exact-string test,
    /// and the module doc says the token never travels through the
    /// environment. The parse error walked past all of it.
    ///
    /// Three shapes, and the order they were found in is why
    /// [`parse_failure`] ended up printing a line number and nothing
    /// else.
    ///
    /// A syntax error quotes the source line, which is usually the
    /// token's. A token written unquoted fails as `invalid type: integer
    /// `...``, with the value inside `message()` rather than inside the
    /// quoted source. And a token pasted onto the wrong key fails as
    /// `invalid type: string "..."`, on a line that does not mention
    /// `token` at all, so no rule that reads the line can see it. There
    /// is a row for every other field in `Section`, because the shape
    /// holds whatever the field's type is: the integer and bool ones
    /// quote the string back through `toml`, and the three duration ones
    /// parse as TOML and are refused a step later by `parse_duration`.
    #[test]
    fn a_section_that_does_not_parse_never_says_what_is_on_the_token_line() {
        for (toml, secret) in [
            (
                "token = \"MTIzNDU2Nzg5.SUPERSECRET.abcdef\" trailing garbage\nguild_id = 1\n",
                "SUPERSECRET",
            ),
            ("token = 99SECRETNUM88\nguild_id = 1\n", "SECRETNUM"),
            ("token = 9912345678\nguild_id = 1\n", "9912345678"),
            (
                "guild_id = 1\ntoken = \"MTIz.ANOTHERSECRET.xyz\" oops\n",
                "ANOTHERSECRET",
            ),
            // A token pasted onto the wrong key, which is the shape
            // neither earlier rule caught: the value is a well formed
            // string on a line that does not mention `token`, so
            // nothing about the line gives it away. Every field in
            // `Section` except `token` is a `u64`, `usize` or `bool`,
            // so all of them quote a pasted string back the same way.
            (
                "token = \"t\"\nguild_id = \"MTIz.WRONGKEY.abc\"\n",
                "WRONGKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\nlog_channel = \"MTIz.CHANNELKEY.abc\"\n",
                "CHANNELKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\nmonitor_channel = \"MTIz.MONITORKEY.abc\"\n",
                "MONITORKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\nerr_channel = \"MTIz.ERRKEY.abc\"\n",
                "ERRKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\nbuffer_lines = \"MTIz.LINESKEY.abc\"\n",
                "LINESKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\nignore_dogs = \"MTIz.DOGSKEY.abc\"\n",
                "DOGSKEY",
            ),
            // The three duration fields are `String` in `Section`, so a
            // pasted token parses as TOML and is refused by
            // `parse_duration` instead. That is a different code path to
            // every row above and it reaches the same operator log.
            (
                "token = \"t\"\nguild_id = 1\nflush = \"MTIz.FLUSHKEY.abc\"\n",
                "FLUSHKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\ncoalesce = \"MTIz.COALESCEKEY.abc\"\n",
                "COALESCEKEY",
            ),
            (
                "token = \"t\"\nguild_id = 1\nmonitor_interval = \"MTIz.INTERVALKEY.abc\"\n",
                "INTERVALKEY",
            ),
        ] {
            let err = Config::from_toml(toml).expect_err(toml).to_string();
            assert!(!err.contains(secret), "the token reached the error: {err}");
            assert!(matches!(
                Config::from_toml(toml).expect_err(toml),
                Error::Config(_)
            ));
        }
    }

    /// The line number survives, since it is the whole of what is left to
    /// diagnose with, and a failure that names no line at all would trade
    /// one unusable error for another.
    #[test]
    fn a_parse_failure_still_names_the_line_it_failed_on() {
        let err = Config::from_toml("token = \"t\"\nguild_id = 1\nbuffer_lines = = 2\n")
            .expect_err("refused")
            .to_string();
        assert!(err.contains("line 3"), "{err}");

        let on_the_token_line =
            Config::from_toml("token = \"MTIz.SECRET.abc\" oops\nguild_id = 1\n")
                .expect_err("refused")
                .to_string();
        assert!(on_the_token_line.contains("line 1"), "{on_the_token_line}");
        assert!(
            on_the_token_line.contains("bot token"),
            "it says why it is being terse: {on_the_token_line}"
        );
        crate::test_support::assert_no_dashes(&on_the_token_line);
    }

    /// Which variant a failure answers with is what decides whether the
    /// run loop stays up or stops, so it is pinned here rather than left
    /// to whoever reads the message. Every failure this function can
    /// produce is listed, driven through real TOML rather than built by
    /// hand: a check added here without a row of its own fails this test
    /// instead of quietly picking a side.
    #[test]
    fn a_section_that_is_wrong_is_told_apart_from_one_nobody_filled_in() {
        for wrong in [
            "this is not TOML at all",
            "token = \"t\"\nguild_id = 1\nbuffer_line = 10\n",
            "token = \"t\"\nguild_id = 0\n",
            "token = \"t\"\nguild_id = 1\nmonitor_channel = 0\n",
            "token = \"t\"\nguild_id = 1\nlog_channel = 0\n",
            "token = \"t\"\nguild_id = 1\nerr_channel = 0\n",
            "token = \"t\"\nguild_id = 1\nflush = \"whenever\"\n",
            "token = \"t\"\nguild_id = 1\ncoalesce = \"whenever\"\n",
            "token = \"t\"\nguild_id = 1\nmonitor_interval = \"whenever\"\n",
            "token = \"t\"\nguild_id = 1\nbuffer_lines = 0\n",
        ] {
            let err = Config::from_toml(wrong).expect_err(wrong);
            assert!(matches!(err, Error::Config(_)), "{wrong}: {err:?}");
        }

        // A section that is both: a value this dog refuses, and a key
        // nobody has typed. Wrong wins, because fixing only the absent
        // key would not make it run. These four answered `Unconfigured`
        // until CodeRabbit caught the ordering on
        // shep-pm/shep-discord#3, which left the run loop online on a
        // value it exists to exit for.
        for both in [
            "guild_id = 0\n",
            "guild_id = 1\nflush = \"garbage\"\n",
            "token = \"t\"\nbuffer_lines = 0\n",
            "token = \"t\"\nlog_channel = 0\n",
        ] {
            let err = Config::from_toml(both).expect_err(both);
            assert!(matches!(err, Error::Config(_)), "{both}: {err:?}");
        }

        for unfilled in ["", "guild_id = 1\n", "token = \"t\"\n"] {
            let err = Config::from_toml(unfilled).expect_err(unfilled);
            assert!(matches!(err, Error::Unconfigured(_)), "{unfilled}: {err:?}");
        }
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
    fn a_zero_buffer_lines_is_refused_rather_than_clamped() {
        // A buffer with capacity 0 cannot hold the line it was just handed,
        // so an operator who typed 0 wanted something other than what this
        // dog would do with it. Refusing rather than clamping is this
        // project's stance on every out-of-range config value: an operator
        // who wrote it should be told, not silently corrected.
        let err = Config::from_toml("token = \"t\"\nguild_id = 1\nbuffer_lines = 0\n")
            .expect_err("refused")
            .to_string();
        assert!(err.contains("buffer_lines"), "{err}");
    }

    #[test]
    fn a_zero_guild_id_is_refused_rather_than_accepted() {
        // A Discord snowflake is never 0, so a 0 here is a placeholder an
        // operator forgot to replace, not a real guild. Refusing it now
        // means finding out here rather than once a command registers
        // against a guild that does not exist.
        let err = Config::from_toml("token = \"t\"\nguild_id = 0\n")
            .expect_err("refused")
            .to_string();
        assert!(err.contains("guild_id"), "{err}");
    }

    #[test]
    fn a_zero_channel_is_refused_but_an_unset_one_is_fine() {
        for field in ["monitor_channel", "log_channel", "err_channel"] {
            let err = Config::from_toml(&format!("token = \"t\"\nguild_id = 1\n{field} = 0\n"))
                .expect_err("refused")
                .to_string();
            assert!(err.contains(field), "{err}");
        }

        let config =
            Config::from_toml("token = \"t\"\nguild_id = 1\n").expect("no channel is legitimate");
        assert_eq!(config.monitor_channel, None);
        assert_eq!(config.log_channel, None);
        assert_eq!(config.err_channel, None);
    }

    /// The refusal is the point, and `deny_unknown_fields` on [`Section`]
    /// is what does it: a misspelled key that parsed and was ignored
    /// would leave an operator watching a setting they wrote have no
    /// effect, with nothing anywhere saying why.
    ///
    /// This used to assert the error named `buffer_line`, and
    /// [`parse_failure`] no longer lets it. The key name is not itself a
    /// secret, but the message that carries it is the same `message()`
    /// that quotes a pasted bot token, and no rule told the two apart
    /// across three attempts. The line number is what is left, so that
    /// is what this asserts.
    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let err = Config::from_toml("token = \"t\"\nguild_id = 1\nbuffer_line = 10\n")
            .expect_err("refused")
            .to_string();
        assert!(err.contains("line 3"), "{err}");
        assert!(!err.contains("buffer_line"), "{err}");
    }

    // `Config::from_toml("token = \"t\"\nguild_id = 1\n")` against four
    // hardcoded constants never reads `PRINT_CONFIG` at all, so it cannot
    // fail for the reason it exists: a `PRINT_CONFIG` that drifted from the
    // defaults would still pass. This round trip reparses the block itself.
    #[test]
    fn every_value_the_printed_block_documents_is_the_value_the_code_uses() {
        // PRINT_CONFIG has three kinds of line: the `[discord]` header,
        // prose comments (`# ` with a space), and commented settings
        // (`#key = value`, no space). Uncomment only the settings.
        let uncommented: Vec<&str> = PRINT_CONFIG
            .lines()
            .filter_map(|line| line.strip_prefix('#'))
            .filter(|rest| !rest.starts_with(' '))
            .collect();

        // The guard needs its own guard: a filter that matched nothing
        // would make the round trip below vacuous, parsing an empty string
        // and passing for the wrong reason.
        assert_eq!(
            uncommented.len(),
            10,
            "expected one line per setting, got {uncommented:?}"
        );

        let config =
            Config::from_toml(&uncommented.join("\n")).expect("the printed block is valid");

        // `token` and `guild_id` have no real default, so the block
        // documents a placeholder rather than one; everything else must be
        // exactly what `Config::from_toml` already does on its own.
        assert_eq!(
            config,
            Config {
                token: "your-bot-token".to_owned(),
                guild_id: 123_456_789_012_345_678,
                monitor_channel: Some(123_456_789_012_345_678),
                monitor_interval: Some("1m".parse().expect("a spelling shep accepts")),
                log_channel: Some(123_456_789_012_345_678),
                err_channel: Some(123_456_789_012_345_678),
                flush: UpDuration::from_millis(DEFAULT_FLUSH_MS),
                coalesce: UpDuration::from_millis(DEFAULT_COALESCE_MS),
                buffer_lines: DEFAULT_BUFFER_LINES,
                ignore_dogs: false,
            }
        );
    }

    /// The schema's property names, sorted.
    fn schema_keys() -> Vec<String> {
        let schema = shep_client::dogs::config_schema::<Section>().expect("publishable");
        let mut keys: Vec<String> = schema
            .as_value()
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("an object schema has properties")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    // A one-way `contains` check, checking only that the schema's keys
    // appear in `PRINT_CONFIG`, passes when `PRINT_CONFIG` also carries a
    // stale or misspelled key alongside the real one: the real key is
    // still there to match against, so a renamed field leaves a dangling
    // line and the test never notices. Two-way set equality catches that
    // side too.
    #[test]
    fn the_schema_and_the_printed_block_name_the_same_settings() {
        let mut printed: Vec<String> = PRINT_CONFIG
            .lines()
            .filter_map(|line| line.strip_prefix('#'))
            .filter(|rest| !rest.starts_with(' '))
            .filter_map(|setting| setting.split_once(' '))
            .map(|(key, _)| key.to_owned())
            .collect();
        printed.sort();

        assert_eq!(printed.len(), 10, "one key per setting, got {printed:?}");
        assert_eq!(schema_keys(), printed);
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(PRINT_CONFIG);
    }
}
