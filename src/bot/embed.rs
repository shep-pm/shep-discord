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
    limits::{self, EMBED_TITLE_LIMIT, FIELD_VALUE_LIMIT},
    shepherd::Verb,
};

/// Discord's own ceiling on a component's `custom_id`, in characters.
#[allow(
    dead_code,
    reason = "read by custom_id's own debug_assert; unreached from main until Task 13's /monitor draws a sheep's buttons, one sheep per message"
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
    reason = "called by custom_id; unreached from main until Task 13's /monitor draws a sheep's buttons, one sheep per message"
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
    reason = "called by process_buttons and Task 13's rediscovery, which reads a sheep id back out of a button's custom_id; unreached from main until then"
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
fn embed_title(name: &str) -> String {
    limits::fit(name, EMBED_TITLE_LIMIT)
}

/// `cpu_percent`'s reading, or the word for why there is not one:
/// [`ProcessInfo::cpu_percent`] is `None` while stopped, freshly started,
/// or reported by a daemon too old to sample it, and none of those three
/// is a zero.
fn cpu_value(cpu_percent: Option<f32>) -> String {
    cpu_percent.map_or_else(|| "unknown".to_owned(), |percent| format!("{percent:.1}%"))
}

/// `memory_bytes`'s reading, on [`cpu_value`]'s own terms.
fn memory_value(memory_bytes: Option<u64>) -> String {
    memory_bytes.map_or_else(
        || "unknown".to_owned(),
        |bytes| MemSize::from_bytes(bytes).to_string(),
    )
}

/// The OS pid, or `N/A` while the sheep is not running.
fn pid_value(pid: Option<u32>) -> String {
    pid.map_or_else(|| "N/A".to_owned(), |pid| pid.to_string())
}

/// One lamb per `name (pid)`, joined with commas; `none` for a walk that
/// found no descendants.
///
/// Capped at [`FIELD_VALUE_LIMIT`]: `lamb.name` comes from the OS process
/// table, and shep does not bound either a process's own name or how many
/// descendants a walk can find, so the joined string this builds is exactly
/// as unbounded as an operator-chosen fold or a sheep's own name.
fn lambs_value(lambs: &[Lamb]) -> String {
    if lambs.is_empty() {
        return "none".to_owned();
    }
    let joined = lambs
        .iter()
        .map(|lamb| format!("{} ({})", lamb.name, lamb.pid))
        .collect::<Vec<_>>()
        .join(", ");
    limits::fit(&joined, FIELD_VALUE_LIMIT)
}

/// Where a dog came from, on [`shep_client::shep_core::protocol::DogSource`]'s own two cases.
///
/// Capped at [`FIELD_VALUE_LIMIT`]: [`DogSource::Adopted`]'s `path` is
/// whatever an operator handed `shep adopt`, with no length rule in
/// shep-core's config validation or the daemon, so it is exactly as
/// unbounded as a sheep's own name.
fn dog_value(dog: &DogSource) -> String {
    match dog {
        DogSource::BuiltIn => "built in".to_owned(),
        DogSource::Adopted { path } => limits::fit(&format!("adopted: {path}"), FIELD_VALUE_LIMIT),
        // `DogSource` is `#[non_exhaustive]`: a future variant renders here
        // rather than failing to compile against an older shep-client.
        _ => "unknown".to_owned(),
    }
}

/// The ordered `(name, value)` field list a sheep's embed draws: seven
/// always, Status through Sheep ID, and six more only when `info` carries
/// the value, Instance, Lambs, Fold, Smit, Dog, and Dog Stale, the last
/// only when `dog_stale` is `Some(true)`, since `Some(false)` and `None`
/// both mean there is nothing an operator needs to see.
///
/// [`process_embed`] and [`embed_character_count`] both fold over this one
/// list rather than each carrying its own copy of it: a restated character
/// budget already drifted from what this module actually built once,
/// before `embed_worst_case_field_arithmetic_stays_under_budget` pinned
/// the doc comment below against the built embed's own JSON instead of a
/// hand count, and two field lists in two functions is the same drift
/// waiting to happen again the moment one of them changes without the
/// other.
fn fields(info: &ProcessInfo) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("Status", info.status.to_string()),
        (
            "Uptime",
            UpDuration::from_millis(info.uptime_ms).to_string(),
        ),
        ("CPU", cpu_value(info.cpu_percent)),
        ("Memory", memory_value(info.memory_bytes)),
        ("Restarts", info.restarts.to_string()),
        ("PID", pid_value(info.pid)),
        ("Sheep ID", info.id.to_string()),
    ];

    if let Some(instance) = info.instance {
        fields.push(("Instance", instance.to_string()));
    }
    if let Some(lambs) = &info.lambs {
        fields.push(("Lambs", lambs_value(lambs)));
    }
    if let Some(fold) = &info.fold {
        fields.push(("Fold", limits::fit(fold, FIELD_VALUE_LIMIT)));
    }
    if let Some(smit) = &info.smit {
        fields.push(("Smit", limits::fit(smit, FIELD_VALUE_LIMIT)));
    }
    if let Some(dog) = &info.dog {
        fields.push(("Dog", dog_value(dog)));
    }
    if info.dog_stale == Some(true) {
        fields.push(("Dog Stale", "yes".to_owned()));
    }

    fields
}

/// Render one sheep as a [`CreateEmbed`].
///
/// Green when [`ProcessInfo::status`] is [`ProcStatus::Online`], red
/// otherwise. See [`fields`] for which of the thirteen possible fields a
/// given `info` draws.
///
/// Never carries a field for `version`, `namespace`, `exec_mode`,
/// `max_memory_restart`, `autorestart`, or `interpreter`: none of the six
/// has a source in [`ProcessInfo`], and this embed is shep-native rather
/// than a PM2 embed with holes papered over with "Unknown".
///
/// # Why every field still fits under one message's budget
///
/// [`crate::limits::MESSAGE_CHARACTER_BUDGET`] is a per-message sum, and
/// this function only ever builds one embed, so its own worst case is that
/// embed's title plus every field name and value it can produce, all
/// present at once. Four of the thirteen fields (Lambs, Fold, Smit, Dog)
/// are built from a `String` shep does not bound, so each is capped at
/// [`FIELD_VALUE_LIMIT`] by [`lambs_value`], [`dog_value`], or a direct
/// [`limits::fit`] call; the other nine are either a fixed word or a
/// number from a type with a known widest form, so they need no cap. The
/// worst case, characters:
///
/// | piece | width |
/// |---|---|
/// | title (name, capped by [`embed_title`]) | 256 |
/// | 13 field names (`"Status"` through `"Dog Stale"`) | 73 |
/// | `Status` value, longest [`ProcStatus`] word (`"waiting-restart"`) | 15 |
/// | `Uptime` value, `u64::MAX` milliseconds with no exact unit | 20 |
/// | `CPU` value, `format!("{:.1}%", f32::MIN)` | 43 |
/// | `Memory` value, `u64::MAX` bytes with no exact unit | 20 |
/// | `Restarts`, `PID`, `Sheep ID` values, three `u32::MAX`s | 30 |
/// | `Instance` value, `u32::MAX` | 10 |
/// | `Dog Stale` value, `"yes"` | 3 |
/// | `Lambs`, `Fold`, `Smit`, `Dog` values, four at [`FIELD_VALUE_LIMIT`] | 4096 |
///
/// `256 + 73 + 15 + 20 + 43 + 20 + 30 + 10 + 3 + 4096 = 4566`, under
/// [`crate::limits::MESSAGE_CHARACTER_BUDGET`]'s 6,000 with 1,434 to
/// spare. That margin is why [`FIELD_VALUE_LIMIT`] keeps Discord's own
/// 1,024 ceiling rather than a lower one: only four of the thirteen
/// fields need a cap at all, so there is room for every one of them at
/// the full ceiling and still land well clear of the budget.
/// `embed_worst_case_field_arithmetic_stays_under_budget` builds exactly
/// this case and checks the built embed's own JSON, not this restated
/// number.
///
/// This is still one embed's own worst case, not the message it rides on.
/// [`crate::bot::commands::shep`]'s `/shep list` is the first caller that
/// can put more than one of these on a message, and it sums
/// [`embed_character_count`] across every sheep it draws to keep that true
/// aggregate under budget rather than trusting this margin twice over.
pub fn process_embed(info: &ProcessInfo) -> CreateEmbed {
    let colour = if info.status == ProcStatus::Online {
        Colour::DARK_GREEN
    } else {
        Colour::RED
    };

    let mut embed = CreateEmbed::new()
        .title(embed_title(&info.name))
        .colour(colour);
    for (name, value) in fields(info) {
        embed = embed.field(name, value, true);
    }
    embed
}

/// The character cost [`process_embed`] would add to one Discord message:
/// its title plus every field name and value [`fields`] builds.
///
/// A caller stacking more than one sheep's embed onto one followup, the
/// way `/shep list` does, needs this to decide how many fit before Discord
/// decides for it with a 400: [`crate::limits::MESSAGE_CHARACTER_BUDGET`]
/// is a sum across every embed on the message, and [`process_embed`]'s own
/// doc comment only works out one embed's own worst case.
///
/// Recomputes [`fields`] rather than serializing the built embed back to
/// JSON to measure it: `serde_json` is a dev-dependency of this crate, for
/// its own tests, and pulling it into the shipped binary just to read a
/// character count back out of a value this function already knows how to
/// build would be a second, slower way to ask a question [`fields`]
/// already answers.
#[must_use]
pub fn embed_character_count(info: &ProcessInfo) -> usize {
    let title = embed_title(&info.name).chars().count();
    let drawn: usize = fields(info)
        .iter()
        .map(|(name, value)| name.chars().count() + value.chars().count())
        .sum();
    title + drawn
}

/// The five action buttons an operator gets on a sheep's embed, one row,
/// styled the way the old embed did: Start success, Stop secondary,
/// Restart secondary, Delete danger, Flush primary.
///
/// Five is [`CreateActionRow`]'s own ceiling, not a number chosen here: a
/// sixth verb needs a second row, never a sixth button on this one.
#[allow(
    dead_code,
    reason = "called by Task 13's /monitor, which draws one sheep per message so its buttons never share a message's five-action-row cap with another sheep's; /shep list (Task 12) draws no buttons, since it packs several sheep per message on the character budget alone"
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
    ///
    /// `info(1, "web")` alone leaves `instance`, `lambs`, `fold`, `smit`,
    /// `dog`, and `dog_stale` all `None`, so those six fields never appear
    /// and this check never reaches them. The builder call below fills
    /// every optional field so all thirteen field names, and every
    /// fixed-word value among them, reach [`crate::test_support::assert_no_dashes`].
    /// [`DogSource::Adopted`] is checked on a second embed, since `dog` only
    /// ever holds one variant at a time.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        let built_in = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .instance(Some(0))
            .lambs(Some(vec![Lamb::new(2, "child")]))
            .fold(Some("main".to_owned()))
            .smit(Some("smit".to_owned()))
            .dog(Some(DogSource::BuiltIn))
            .dog_stale(Some(true))
            .build();

        let adopted = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .dog(Some(DogSource::Adopted {
                path: "/usr/local/bin/watchdog".to_owned(),
            }))
            .build();

        for candidate in [built_in, adopted] {
            let embed = process_embed(&candidate);
            let json = serde_json::to_value(&embed).expect("json");
            let title = json["title"].as_str().expect("title");
            crate::test_support::assert_no_dashes(title);
            for field in json["fields"].as_array().expect("fields") {
                crate::test_support::assert_no_dashes(field["name"].as_str().expect("name"));
                crate::test_support::assert_no_dashes(field["value"].as_str().expect("value"));
            }
        }
    }

    /// `Debug` is not a stable contract and a substring cannot tell a
    /// present field from the same text appearing anywhere else in the
    /// dump, so this reads the embed's own JSON `fields` array instead, the
    /// way [`a_sheep_name_past_the_title_limit_is_truncated_not_refused`]
    /// and [`embed_worst_case_field_arithmetic_stays_under_budget`] already
    /// do.
    #[test]
    fn dog_stale_only_shows_up_when_true() {
        let has_dog_stale_field = |dog_stale: Option<bool>| {
            let mut candidate = info(1, "web");
            candidate.dog_stale = dog_stale;
            let json = serde_json::to_value(process_embed(&candidate)).expect("json");
            json["fields"]
                .as_array()
                .expect("fields")
                .iter()
                .any(|field| field["name"].as_str() == Some("Dog Stale"))
        };

        assert!(has_dog_stale_field(Some(true)));
        assert!(!has_dog_stale_field(Some(false)));
        assert!(!has_dog_stale_field(None));
    }

    /// The worst case [`process_embed`]'s own doc comment works out by
    /// hand: every optional field present, the four built from an
    /// unbounded `String` (Lambs, Fold, Smit, Dog) each long enough to hit
    /// [`FIELD_VALUE_LIMIT`], and every numeric field at its type's widest
    /// form. `cpu_percent` uses `f32::MIN` rather than `f32::MAX`:
    /// shep-core does not constrain it non-negative, and the sign in
    /// `format!("{:.1}%", f32::MIN)` costs a character `f32::MAX` does not,
    /// so `f32::MIN` is the true widest render. Reads the built embed's own
    /// JSON, the way Discord would see it, and checks the sum against
    /// [`crate::limits::MESSAGE_CHARACTER_BUDGET`] itself rather than a
    /// number restated from it.
    #[test]
    fn embed_worst_case_field_arithmetic_stays_under_budget() {
        let long_name = "a".repeat(EMBED_TITLE_LIMIT + 50);
        let mut worst = info(u32::MAX, &long_name);
        worst.status = ProcStatus::WaitingRestart;
        worst.pid = Some(u32::MAX);
        worst.restarts = u32::MAX;
        worst.uptime_ms = u64::MAX;
        worst.cpu_percent = Some(f32::MIN);
        worst.memory_bytes = Some(u64::MAX);
        worst.instance = Some(u32::MAX);
        worst.lambs = Some(vec![Lamb::new(u32::MAX, "x".repeat(2_000))]);
        worst.fold = Some("f".repeat(2_000));
        worst.smit = Some("s".repeat(2_000));
        worst.dog = Some(DogSource::Adopted {
            path: "p".repeat(2_000),
        });
        worst.dog_stale = Some(true);

        let embed = process_embed(&worst);
        let json = serde_json::to_value(&embed).expect("json");
        let title_len = json["title"].as_str().expect("title").chars().count();
        let field_len: usize = json["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|field| {
                field["name"].as_str().expect("name").chars().count()
                    + field["value"].as_str().expect("value").chars().count()
            })
            .sum();

        for value_field in ["Lambs", "Fold", "Smit", "Dog"] {
            let width = json["fields"]
                .as_array()
                .expect("fields")
                .iter()
                .find(|field| field["name"].as_str() == Some(value_field))
                .and_then(|field| field["value"].as_str())
                .expect("field present")
                .chars()
                .count();
            assert!(
                width <= FIELD_VALUE_LIMIT,
                "{value_field} value is {width} chars, over FIELD_VALUE_LIMIT"
            );
        }

        assert!(
            title_len + field_len <= limits::MESSAGE_CHARACTER_BUDGET,
            "{} over the message budget",
            title_len + field_len
        );
    }

    /// [`embed_character_count`] and [`process_embed`] both fold over
    /// [`fields`], so this pins them against each other rather than
    /// against a third hand count: the built embed's own JSON is what
    /// Discord actually sees, and `embed_character_count` exists so a
    /// caller packing several sheep onto one message never has to build
    /// the embed first just to learn its size.
    #[test]
    fn embed_character_count_matches_the_built_embeds_own_json() {
        let worst = crate::test_support::worst_case_sample(u32::MAX);

        let json = serde_json::to_value(process_embed(&worst)).expect("json");
        let title_len = json["title"].as_str().expect("title").chars().count();
        let field_len: usize = json["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|field| {
                field["name"].as_str().expect("name").chars().count()
                    + field["value"].as_str().expect("value").chars().count()
            })
            .sum();

        assert_eq!(embed_character_count(&worst), title_len + field_len);
    }
}
