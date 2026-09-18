//! Discord's own length limits, named once, and the truncation they share.
//!
//! # Why this file exists for one small function
//!
//! This port has now been bitten by a Discord length limit four times:
//! [`crate::bot::embed::custom_id`] found that a `custom_id` caps at 100
//! characters, so a button keys on a numeric sheep id rather than a name;
//! [`crate::stream::pack`] exists because the 6,000 character sum across
//! every embed on one message is real, and treating it as an assumption
//! rather than an enforced budget cost a whole fix round; the embed
//! title's 256 character cap was caught in
//! [`crate::bot::embed`] before this file existed; and
//! [`crate::bot::embed::process_embed`] put an operator-chosen fold or smit
//! into a field with no cap at all, the same shape of gap as an uncapped
//! sheep name. Four encounters with one class of bug is a missing
//! abstraction, not four unrelated ones, so the caps live here once and
//! [`fit`] is the one place a string gets cut down to one of them.
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
}
