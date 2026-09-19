//! Discord's own length limits, named once, and the truncation they share.
//!
//! # Why this file exists for one small function
//!
//! Nine Discord limits have bitten this port. They are listed rather than
//! counted in a sentence, because a running tally has to be renumbered
//! every time one is added and the tenth is then one more line instead of
//! a re-count of this paragraph:
//!
//! - `custom_id` caps at 100 characters, so a button keys on a numeric
//!   sheep id rather than a name. The cap stays local to
//!   [`crate::bot::embed`]; see below.
//! - [`MESSAGE_CHARACTER_BUDGET`], the 6,000 character sum across every
//!   embed on ONE message. [`crate::stream::pack`] exists for it, and
//!   treating it as an assumption rather than an enforced budget cost a
//!   whole fix round. `/shep list` is the second caller to need it.
//! - [`EMBED_TITLE_LIMIT`], caught in [`crate::bot::embed`] before this
//!   file existed.
//! - [`FIELD_VALUE_LIMIT`]: [`crate::bot::embed::process_embed`] put an
//!   operator-chosen fold or smit into a field with no cap at all, the
//!   same shape of gap as an uncapped sheep name.
//! - [`EMBED_DESCRIPTION_LIMIT`], for the same reason one field down.
//! - [`MESSAGE_CONTENT_LIMIT`]: an error's `Display` went straight into a
//!   followup's content with no bound of its own.
//! - [`EMBED_MAX_COUNT`], ten embeds on one message. A listing of many
//!   small sheep reaches it long before it reaches the character sum, so
//!   a packer watching only characters would take a 400 for the count.
//! - Discord's 25 suggestions per autocomplete response, kept local to
//!   `/shep` since nothing else in this crate offers suggestions.
//! - [`AUTOCOMPLETE_CHOICE_NAME_LIMIT`], the one case in this file where
//!   cutting a string is the wrong answer; see its own doc comment.
//! - The four slash command registration caps, [`COMMAND_NAME_LIMIT`],
//!   [`COMMAND_DESCRIPTION_LIMIT`], [`OPTION_NAME_LIMIT`] and
//!   [`OPTION_DESCRIPTION_LIMIT`], which apply at every nesting depth.
//!   These have the worst failure mode of the ten and are the only ones
//!   [`fit`] must NOT be used on; both halves of that are argued below.
//!
//! Ten encounters with one class of bug is a missing abstraction, not ten
//! unrelated ones, so the caps live here once and [`fit`] is the one
//! place a string gets cut down to one of them.
//!
//! # Why the registration caps are the dangerous ones
//!
//! Every other limit here spoils one message. These four take the whole
//! bot's command surface out of a guild at once.
//!
//! `GuildId::set_commands` sends every command in one payload, so one
//! description a character too long is a 400 for all of them.
//! [`crate::bot::command::register`] prints one line to stderr and
//! carries on; the gateway stays up, log streaming and the monitor keep
//! working, and nothing looks broken. Meanwhile `/shep`, `/system` and
//! `/monitor` have all silently vanished from the guild, and one
//! over-long option description on one subcommand is enough to do it.
//!
//! So these are asserted rather than fitted, which is the opposite of
//! every other cap in this file. A truncated sheep name is a cosmetic
//! loss in a message somebody is reading; a truncated command
//! description is a permanent lie in Discord's own UI about what a
//! command does. These strings are written by whoever edits this crate,
//! not supplied by an operator, so the right place to catch one is a
//! failing test before it ships.
//!
//! `custom_id`'s own 100 character cap stays where it is, in
//! [`crate::bot::embed`]: nothing there needs truncating, since
//! [`crate::bot::embed::custom_id`] builds its id from a fixed short word
//! and a `u32`, never from an unbounded string. This module is for a limit
//! that a real, operator-supplied string can actually exceed.

/// The longest an embed's `title` may be. Discord's own limit.
pub const EMBED_TITLE_LIMIT: usize = 256;

/// The longest an embed's `description` may be. Discord's own limit.
pub const EMBED_DESCRIPTION_LIMIT: usize = 4096;

/// The longest an embed's field `value` this crate lets through, in
/// characters.
///
/// Discord's own ceiling on a field value is 1,024, and this crate keeps
/// that ceiling rather than lowering it: see
/// [`crate::bot::embed`]'s module doc for the arithmetic proving every
/// field [`crate::bot::embed::process_embed`] can build, all present at
/// once and each at this cap, still sums under [`MESSAGE_CHARACTER_BUDGET`]
/// with room to spare. A lower cap is only needed when the arithmetic
/// says so, and here it does not.
pub const FIELD_VALUE_LIMIT: usize = 1024;

/// The character budget for everything counted, summed across every embed
/// on one message: every `title`, `description`, `field.name`,
/// `field.value`, `footer.text` and `author.name`. Discord's own limit.
pub const MESSAGE_CHARACTER_BUDGET: usize = 6000;

/// The most embeds one message may carry. Discord's own limit, and a count
/// rather than a length, but it belongs beside [`MESSAGE_CHARACTER_BUDGET`]
/// for the same reason: a packer that only watches the character sum can
/// still stack eleven small embeds onto one message and take a 400 for the
/// count alone, the same shape of gap a length-only check would leave.
pub const EMBED_MAX_COUNT: usize = 10;

/// The longest an autocomplete choice's `name` (and, since serenity's
/// `AutocompleteChoice::from` sets both from the same string, its `value`
/// too) may be. Discord's own limit, and the ninth this port has run
/// into.
///
/// Not fed to [`fit`]: a truncated `value` is what Discord sends back as
/// the argument once an operator picks the suggestion, so a truncated
/// name would offer a sheep that does not exist under that shortened
/// name, and the verb would fail against it. A name that cannot be sent
/// whole cannot be offered as a working suggestion at all, so
/// [`crate::bot::commands::shep::ShepCommand::suggestions`] drops it
/// instead of shortening it, keeping every other suggestion in the same
/// response alive. Discord answers one autocomplete response with every
/// suggestion in a single payload, so a single over-length name, if sent,
/// would fail the whole response and silently empty every suggestion for
/// that keystroke, not just the one that was too long.
pub const AUTOCOMPLETE_CHOICE_NAME_LIMIT: usize = 100;

/// The longest a slash command's `name` may be. Discord's own limit, and
/// the same cap an option's name carries; see [`OPTION_NAME_LIMIT`] for
/// why the two are named separately anyway.
#[allow(
    dead_code,
    reason = "read by test_support::assert_registration_lengths, which is the only enforcement these four can have: no shipped code path may fit or truncate a registration string, per this module's own doc, so a test is where they are checked"
)]
pub const COMMAND_NAME_LIMIT: usize = 32;

/// The longest a slash command's `description` may be. Discord's own
/// limit.
#[allow(
    dead_code,
    reason = "read by test_support::assert_registration_lengths, which is the only enforcement these four can have: no shipped code path may fit or truncate a registration string, per this module's own doc, so a test is where they are checked"
)]
pub const COMMAND_DESCRIPTION_LIMIT: usize = 100;

/// The longest a command option's `name` may be, at any nesting depth: a
/// subcommand's own name and the name of an option under it are both
/// options as far as Discord's payload is concerned.
///
/// Numerically the same as [`COMMAND_NAME_LIMIT`] and named separately on
/// purpose. They are two limits that happen to agree, not one limit used
/// twice, and a reader checking one of them against Discord's
/// documentation should not have to work out which.
#[allow(
    dead_code,
    reason = "read by test_support::assert_registration_lengths, which is the only enforcement these four can have: no shipped code path may fit or truncate a registration string, per this module's own doc, so a test is where they are checked"
)]
pub const OPTION_NAME_LIMIT: usize = 32;

/// The longest a command option's `description` may be, at any nesting
/// depth. Discord's own limit, and the one this crate comes closest to:
/// the longest description in the tree today is 70 characters.
#[allow(
    dead_code,
    reason = "read by test_support::assert_registration_lengths, which is the only enforcement these four can have: no shipped code path may fit or truncate a registration string, per this module's own doc, so a test is where they are checked"
)]
pub const OPTION_DESCRIPTION_LIMIT: usize = 100;

/// The longest a message's own `content` field may be, separate from
/// anything an embed carries. Discord's own limit, and the sixth of its
/// length limits this port has run into: an error's `Display` is
/// interpolated straight into a followup's `content` in
/// `crate::bot::interaction::report_failure`, with no bound of its own
/// before this cap was named here.
pub const MESSAGE_CONTENT_LIMIT: usize = 2000;

/// Truncate `text` to at most `limit` characters, ending in a single
/// `'\u{2026}'` ellipsis when it had to cut anything.
///
/// Counts characters, not bytes: Discord counts characters for every one of
/// the limits above, and a byte-based split can cut a multibyte character
/// in half or refuse text Discord would have accepted whole. Text already
/// at or under `limit` comes back unchanged; text over it comes back at
/// exactly `limit` characters, the last of them the ellipsis, so a caller
/// can always trust `fit(text, limit).chars().count() <= limit`. This is
/// the same shape `shep-cli`'s own `lookout::view::flock::layout::fit`
/// uses to fit a name to a terminal column, so a truncated string in this
/// crate reads the same way a truncated one does in shep's other clients.
///
/// `limit == 0` is the one degenerate case: there is no room for even the
/// ellipsis, so this returns an empty string rather than one character
/// over budget.
#[must_use]
pub fn fit(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let truncated: String = text.chars().take(limit - 1).collect();
    format!("{truncated}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn text_within_the_limit_is_unchanged() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 5), "hello");
    }

    #[test]
    fn text_over_the_limit_ends_in_one_ellipsis_at_exactly_the_limit() {
        let fitted = fit(&"x".repeat(20), 10);
        assert_eq!(fitted.chars().count(), 10);
        assert!(fitted.ends_with('\u{2026}'));
        assert_eq!(fitted.matches('\u{2026}').count(), 1);
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        // Each 'é' below is two bytes in UTF-8. A byte-based split would
        // either cut one in half or stop short of the character limit
        // Discord actually allows.
        let fitted = fit(&"é".repeat(20), 10);
        assert_eq!(fitted.chars().count(), 10);
        assert!(fitted.ends_with('\u{2026}'));
    }

    #[test]
    fn a_zero_limit_returns_empty_rather_than_one_character_over() {
        assert_eq!(fit("hello", 0), "");
    }

    /// The other end of the same edge. A limit of 1 leaves room for the
    /// ellipsis and nothing else, so the answer is the ellipsis alone:
    /// `take(limit - 1)` takes nothing and the ellipsis is one character.
    /// The code is plain enough to read off, which is exactly why nobody
    /// had pinned it, and an off-by-one here returns two characters for a
    /// one-character budget, which Discord refuses.
    #[test]
    fn a_limit_of_one_is_the_ellipsis_and_nothing_else() {
        let fitted = fit("hello", 1);
        assert_eq!(fitted, "\u{2026}");
        assert_eq!(fitted.chars().count(), 1);
    }
}
