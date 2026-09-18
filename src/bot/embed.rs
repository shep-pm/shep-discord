//! Rendering one sheep as a Discord embed, and the buttons that act on it.
//!
//! # Why the button id carries a number, never a name
//!
//! The old code this dog ports put the process name straight into a
//! button's `custom_id` (`process.ts:127`), on a comment asserting PM2
//! names cannot carry a colon. Discord caps `custom_id` at
//! [`CUSTOM_ID_LIMIT`] characters, and nothing in shep-core's config
//! validation or the daemon caps a sheep's name, so that scheme is not
//! safe to reuse here: a long enough name would make every button on that
//! sheep's embed a silent 400. [`custom_id`] and [`parse_custom_id`] key on
//! [`shep_client::shep_core::protocol::ProcessInfo::id`] instead, the
//! numeric form [`shep_client::shep_core::protocol::SelectorSpec::Id`]
//! exists to receive, and the verb/id pair this module writes never grows
//! past a couple dozen characters regardless of what a sheep is named.
//!
//! The embed's title has the same shape of gap and a smaller fix:
//! Discord's own limit on an embed title is 256 characters, and a sheep's
//! name is exactly as unbounded there as it is in a `custom_id`. See
//! [`embed_title`].

use serenity::all::{ButtonStyle, Colour, CreateActionRow, CreateButton, CreateEmbed};
use shep_client::shep_core::{
    protocol::{DogSource, Lamb, ProcessInfo},
    status::ProcStatus,
    values::{MemSize, UpDuration},
};

use crate::{
    limits::{self, EMBED_TITLE_LIMIT},
    shepherd::Verb,
};

/// Discord's own ceiling on a component's `custom_id`, in characters.
#[allow(
    dead_code,
    reason = "read by custom_id's own debug_assert; unreached from main until Task 11 wires bot::run to this module"
)]
pub const CUSTOM_ID_LIMIT: usize = 100;

/// One verb, as [`custom_id`] and [`parse_custom_id`] spell it on the wire.
///
/// A `match` rather than [`core::fmt::Display`]: this string is a
/// component identifier a person never reads, not a sentence, and giving
/// [`Verb`] a `Display` impl for it would invite a reply sentence to
/// borrow that spelling too, which is exactly the kind of string this
/// dog's dash check exists for and this one is not meant to pass.
#[allow(
    dead_code,
    reason = "called by custom_id; unreached from main until Task 11 wires bot::run to this module"
)]
fn verb_str(verb: Verb) -> &'static str {
    match verb {
        Verb::Start => "start",
        Verb::Stop => "stop",
        Verb::Restart => "restart",
        Verb::Reload => "reload",
        Verb::Delete => "delete",
        Verb::Flush => "flush",
        Verb::Save => "save",
        Verb::Reopen => "reopen",
    }
}

/// The inverse of [`verb_str`]. `None` for anything else, [`Verb`]
/// included: a match arm here for every variant is what keeps this in
/// lock step with it.
#[allow(
    dead_code,
    reason = "called by parse_custom_id; unreached from main until Task 11 wires bot::run to this module"
)]
fn verb_from_str(raw: &str) -> Option<Verb> {
    Some(match raw {
        "start" => Verb::Start,
        "stop" => Verb::Stop,
        "restart" => Verb::Restart,
        "reload" => Verb::Reload,
        "delete" => Verb::Delete,
        "flush" => Verb::Flush,
        "save" => Verb::Save,
        "reopen" => Verb::Reopen,
        _ => return None,
    })
}

/// Build a button's `custom_id` from a verb and the sheep's numeric id.
///
/// Always well under [`CUSTOM_ID_LIMIT`]: [`verb_str`] returns one of eight
/// fixed short words and a `u32` prints as at most ten digits, so the
/// longest id this can ever produce is `"restart:4294967295"`, 19
/// characters. The `debug_assert!` below is a tripwire against a future
/// verb spelling long enough to change that, not a check this path can
/// fail today.
#[allow(
    dead_code,
    reason = "called by process_buttons and Task 13's rediscovery; unreached from main until Task 11 wires bot::run to this module"
)]
#[must_use]
pub fn custom_id(verb: Verb, id: u32) -> String {
    let id = format!("{}:{id}", verb_str(verb));
    debug_assert!(id.chars().count() <= CUSTOM_ID_LIMIT, "{id}");
    id
}

/// Parse a `custom_id` [`custom_id`] could have written. `None` for
/// anything else, including a well-formed `verb:name` from the old scheme
/// this dog does not carry forward.
#[allow(
    dead_code,
    reason = "called by Task 11's button dispatch and Task 13's rediscovery, unreached from main until then"
)]
#[must_use]
pub fn parse_custom_id(raw: &str) -> Option<(Verb, u32)> {
    let (verb, id) = raw.split_once(':')?;
    let verb = verb_from_str(verb)?;
    let id = id.parse().ok()?;
    Some((verb, id))
}

/// Truncate `name` to fit Discord's embed title limit.
///
/// A truncated title is a worse read than a full one, but a title Discord
/// refuses outright is not a read at all: the embed [`process_embed`]
/// spent the rest of its fields building never reaches the channel either.
#[allow(
    dead_code,
    reason = "called by process_embed; unreached from main until Task 11 wires bot::run to this module"
)]
fn embed_title(name: &str) -> String {
    limits::fit(name, EMBED_TITLE_LIMIT)
}

/// `cpu_percent`'s reading, or the word for why there is not one:
/// [`ProcessInfo::cpu_percent`] is `None` while stopped, freshly started,
/// or reported by a daemon too old to sample it, and none of those three
/// is a zero.
#[allow(
    dead_code,
    reason = "called by process_embed; unreached from main until Task 11 wires bot::run to this module"
)]
fn cpu_value(cpu_percent: Option<f32>) -> String {
    cpu_percent.map_or_else(|| "unknown".to_owned(), |percent| format!("{percent:.1}%"))
}

/// `memory_bytes`'s reading, on [`cpu_value`]'s own terms.
#[allow(
    dead_code,
    reason = "called by process_embed; unreached from main until Task 11 wires bot::run to this module"
)]
fn memory_value(memory_bytes: Option<u64>) -> String {
    memory_bytes.map_or_else(
        || "unknown".to_owned(),
        |bytes| MemSize::from_bytes(bytes).to_string(),
    )
}

/// The OS pid, or `N/A` while the sheep is not running.
#[allow(
    dead_code,
    reason = "called by process_embed; unreached from main until Task 11 wires bot::run to this module"
)]
fn pid_value(pid: Option<u32>) -> String {
    pid.map_or_else(|| "N/A".to_owned(), |pid| pid.to_string())
}

/// One lamb per `name (pid)`, joined with commas; `none` for a walk that
/// found no descendants.
#[allow(
    dead_code,
    reason = "called by process_embed; unreached from main until Task 11 wires bot::run to this module"
)]
fn lambs_value(lambs: &[Lamb]) -> String {
    if lambs.is_empty() {
        "none".to_owned()
    } else {
        lambs
            .iter()
            .map(|lamb| format!("{} ({})", lamb.name, lamb.pid))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Where a dog came from, on [`shep_client::shep_core::protocol::DogSource`]'s own two cases.
#[allow(
    dead_code,
    reason = "called by process_embed; unreached from main until Task 11 wires bot::run to this module"
)]
fn dog_value(dog: &DogSource) -> String {
    match dog {
        DogSource::BuiltIn => "built in".to_owned(),
        DogSource::Adopted { path } => format!("adopted: {path}"),
        // `DogSource` is `#[non_exhaustive]`: a future variant renders here
        // rather than failing to compile against an older shep-client.
        _ => "unknown".to_owned(),
    }
}

/// Render one sheep as a [`CreateEmbed`].
///
/// Green when [`ProcessInfo::status`] is [`ProcStatus::Online`], red
/// otherwise. Seven fields always: Status, Uptime, CPU, Memory, Restarts,
/// PID, Sheep ID. Six more fields, added only when `info` carries the
/// value: Instance, Lambs, Fold, Smit, Dog, and Dog Stale, the last shown
/// only when `dog_stale` is `Some(true)`, since `Some(false)` and `None`
/// both mean there is nothing an operator needs to see.
///
/// Never carries a field for `version`, `namespace`, `exec_mode`,
/// `max_memory_restart`, `autorestart`, or `interpreter`: none of the six
/// has a source in [`ProcessInfo`], and this embed is shep-native rather
/// than a PM2 embed with holes papered over with "Unknown".
#[allow(
    dead_code,
    reason = "called by Task 12's /shep list; unreached from main until then"
)]
pub fn process_embed(info: &ProcessInfo) -> CreateEmbed {
    let colour = if info.status == ProcStatus::Online {
        Colour::DARK_GREEN
    } else {
        Colour::RED
    };

    let mut embed = CreateEmbed::new()
        .title(embed_title(&info.name))
        .colour(colour)
        .field("Status", info.status.to_string(), true)
        .field(
            "Uptime",
            UpDuration::from_millis(info.uptime_ms).to_string(),
            true,
        )
        .field("CPU", cpu_value(info.cpu_percent), true)
        .field("Memory", memory_value(info.memory_bytes), true)
        .field("Restarts", info.restarts.to_string(), true)
        .field("PID", pid_value(info.pid), true)
        .field("Sheep ID", info.id.to_string(), true);

    if let Some(instance) = info.instance {
        embed = embed.field("Instance", instance.to_string(), true);
    }
    if let Some(lambs) = &info.lambs {
        embed = embed.field("Lambs", lambs_value(lambs), true);
    }
    if let Some(fold) = &info.fold {
        embed = embed.field("Fold", fold.clone(), true);
    }
    if let Some(smit) = &info.smit {
        embed = embed.field("Smit", smit.clone(), true);
    }
    if let Some(dog) = &info.dog {
        embed = embed.field("Dog", dog_value(dog), true);
    }
    if info.dog_stale == Some(true) {
        embed = embed.field("Dog Stale", "yes", true);
    }

    embed
}

/// The five action buttons an operator gets on a sheep's embed, one row,
/// styled the way the old embed did: Start success, Stop secondary,
/// Restart secondary, Delete danger, Flush primary.
///
/// Five is [`CreateActionRow`]'s own ceiling, not a number chosen here: a
/// sixth verb needs a second row, never a sixth button on this one.
#[allow(
    dead_code,
    reason = "called by Task 11's interaction dispatch and Task 12's /shep list; unreached from main until then"
)]
pub fn process_buttons(info: &ProcessInfo) -> CreateActionRow {
    let button = |verb: Verb, label: &str, style: ButtonStyle| {
        CreateButton::new(custom_id(verb, info.id))
            .label(label)
            .style(style)
    };

    CreateActionRow::Buttons(vec![
        button(Verb::Start, "Start", ButtonStyle::Success),
        button(Verb::Stop, "Stop", ButtonStyle::Secondary),
        button(Verb::Restart, "Restart", ButtonStyle::Secondary),
        button(Verb::Delete, "Delete", ButtonStyle::Danger),
        button(Verb::Flush, "Flush", ButtonStyle::Primary),
    ])
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::protocol::ProcessInfo;

    use super::*;

    fn info(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    fn button_count(row: &CreateActionRow) -> usize {
        match row {
            CreateActionRow::Buttons(buttons) => buttons.len(),
            CreateActionRow::SelectMenu(_) | CreateActionRow::InputText(_) => 0,
        }
    }

    /// Discord caps a custom_id at 100 characters and shep does not
    /// constrain a sheep name, so the old scheme at `process.ts:127` is
    /// not safe to reuse. `SelectorSpec::Id` exists to receive the numeric
    /// form.
    #[test]
    fn a_custom_id_is_short_whatever_the_sheep_is_called() {
        let id = custom_id(Verb::Restart, u32::MAX);
        assert!(id.chars().count() <= CUSTOM_ID_LIMIT, "{id}");
        assert_eq!(id, "restart:4294967295");
    }

    #[test]
    fn a_custom_id_round_trips() {
        for verb in [
            Verb::Start,
            Verb::Stop,
            Verb::Restart,
            Verb::Delete,
            Verb::Flush,
        ] {
            assert_eq!(parse_custom_id(&custom_id(verb, 7)), Some((verb, 7)));
        }
    }

    #[test]
    fn a_custom_id_this_dog_did_not_write_is_refused() {
        assert_eq!(parse_custom_id("restart"), None);
        assert_eq!(parse_custom_id("restart:web"), None);
        assert_eq!(parse_custom_id("explode:1"), None);
    }

    /// An action row holds at most 5 buttons. The row is exactly at the
    /// ceiling, so a sixth verb needs a second row rather than a silent
    /// 400.
    #[test]
    fn the_button_row_is_within_what_an_action_row_holds() {
        let row = process_buttons(&info(1, "web"));
        assert!(
            button_count(&row) <= 5,
            "an action row holds at most 5 buttons"
        );
    }

    /// Six of the twelve fields the old embed carried have no source in
    /// `ProcessInfo`. Naming them here is the tripwire against somebody
    /// reintroducing an "Unknown" row to fill the gap.
    #[test]
    fn the_embed_carries_no_field_shep_cannot_answer() {
        let rendered = format!("{:?}", process_embed(&info(1, "web")));
        for absent in [
            "Exec Mode",
            "Namespace",
            "Interpreter",
            "Max Memory",
            "Autorestart",
        ] {
            assert!(
                !rendered.contains(absent),
                "{absent} has no ProcessInfo source"
            );
        }
    }

    /// A sheep name past Discord's 256 character title cap would otherwise
    /// take the whole embed down with it, the same shape of gap
    /// `CUSTOM_ID_LIMIT` exists for on the button side.
    #[test]
    fn a_sheep_name_past_the_title_limit_is_truncated_not_refused() {
        let long_name = "a".repeat(EMBED_TITLE_LIMIT + 50);
        let embed = process_embed(&info(1, &long_name));
        let json = serde_json::to_value(&embed).expect("json");
        let title = json["title"].as_str().expect("title");
        assert_eq!(title.chars().count(), EMBED_TITLE_LIMIT);
    }

    /// Every field name and every fixed-word value this module writes,
    /// checked as JSON rather than as a fragile substring, so a rename
    /// that keeps every field but changes its spelling still gets caught
    /// by the dash check on the true set of strings.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        let embed = process_embed(&info(1, "web"));
        let json = serde_json::to_value(&embed).expect("json");
        let title = json["title"].as_str().expect("title");
        crate::test_support::assert_no_dashes(title);
        for field in json["fields"].as_array().expect("fields") {
            crate::test_support::assert_no_dashes(field["name"].as_str().expect("name"));
            crate::test_support::assert_no_dashes(field["value"].as_str().expect("value"));
        }
    }

    #[test]
    fn dog_stale_only_shows_up_when_true() {
        let mut stale_true = info(1, "web");
        stale_true.dog_stale = Some(true);
        let rendered = format!("{:?}", process_embed(&stale_true));
        assert!(rendered.contains("Dog Stale"));

        let mut stale_false = info(1, "web");
        stale_false.dog_stale = Some(false);
        let rendered = format!("{:?}", process_embed(&stale_false));
        assert!(!rendered.contains("Dog Stale"));
    }
}
