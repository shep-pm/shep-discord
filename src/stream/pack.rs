//! Chunking a [`Group`] to Discord's embed limits, and packing chunks onto
//! messages under Discord's real per-message budget.
//!
//! This is where the TypeScript this project ports was wrong in the way
//! that mattered. It capped a batch at ten embeds and sent every pending
//! one in a single call. Discord's actual limit is not a count: it is a
//! 6,000 character sum across `title`, `description`, `field.name`,
//! `field.value`, `footer.text` and `author.name` over every embed on one
//! message. Two full 4,096 character descriptions is already 8,192, so two
//! chunks that would each fit their own message can never share one, and
//! the old code's ten-embed cap did nothing to stop it: it sent the
//! oversized batch, took a 400 back, and retried the same rejected batch
//! forever, because it only cleared its queue on success. [`into_messages`]
//! is the function that makes that failure mode impossible: it packs by
//! the character budget, never by a count of chunks.

use super::buffer::Group;
use crate::limits::{self, EMBED_DESCRIPTION_LIMIT, EMBED_TITLE_LIMIT, MESSAGE_CHARACTER_BUDGET};

/// One embed's worth of a [`Group`]: a title and a description, both under
/// Discord's own per-field limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The sheep's name, alone when the group fit in one chunk, or with a
    /// `" (i/n)"` suffix when [`chunks`] had to split it.
    pub title: String,
    /// At most [`EMBED_DESCRIPTION_LIMIT`] characters of the group's text.
    pub description: String,
}

/// Build a chunk's title from `name` and a `" (i/n)"` `suffix` (empty when
/// the group fit in one chunk), truncating `name` rather than refusing it
/// when the two together would exceed [`EMBED_TITLE_LIMIT`].
///
/// The suffix is the only thing that tells an operator a Discord message
/// was split, so it is never what gets cut: this reserves room for
/// `suffix` first and hands [`limits::fit`] whatever budget is left for
/// `name`, the same order `shep-cli`'s
/// `lookout::view::flock::layout::fit` follows when it fits a name to a
/// terminal column. A log line must never be dropped because a sheep has a
/// long name (the same rule `names.rs` follows when it falls back to
/// `"sheep <id>"`), so a name is shortened rather than the chunk being
/// refused.
fn title(name: &str, suffix: &str) -> String {
    // `saturating_sub` rather than a plain subtraction: a suffix alone at
    // or past the limit is a case this function still has to return
    // something for, even though in practice `" (i/n)"` never approaches
    // 256 characters on its own. `fit` itself reserves the one character
    // its own ellipsis needs, so there is no second `- 1` here.
    let name_budget = EMBED_TITLE_LIMIT.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", limits::fit(name, name_budget))
}

/// Split `group`'s text into one or more [`Chunk`]s, none longer than
/// [`EMBED_DESCRIPTION_LIMIT`] characters, and none with a title longer
/// than [`EMBED_TITLE_LIMIT`] characters.
///
/// Splits on character boundaries, not byte boundaries: Discord counts
/// characters, so a byte based split could cut a multibyte character in
/// half, or refuse a description this limit actually allows. A group that
/// fits in one chunk gets a bare title; a group that needs more than one
/// gets `"<name> (i/n)"` on each, so an operator reading a busy channel can
/// tell a split message from an unrelated one under the same sheep. shep
/// places no limit on a sheep's name, so the title built from it is capped
/// here, by [`title`], rather than assumed to already fit: this is what
/// makes [`into_messages`]'s own budget invariant true.
#[must_use]
pub fn chunks(group: &Group) -> Vec<Chunk> {
    let characters: Vec<char> = group.text.chars().collect();
    let pieces: Vec<String> = characters
        .chunks(EMBED_DESCRIPTION_LIMIT)
        .map(|piece| piece.iter().collect())
        .collect();

    let total = pieces.len();
    pieces
        .into_iter()
        .enumerate()
        .map(|(index, description)| {
            let suffix = if total > 1 {
                format!(" ({}/{total})", index + 1)
            } else {
                String::new()
            };
            Chunk {
                title: title(&group.name, &suffix),
                description,
            }
        })
        .collect()
}

/// Group `items` into batches, each summing to at most `budget` under
/// `size` and holding at most `max_count` items.
///
/// Walks `items` in order, keeping a running sum of `size` for the batch
/// being built, and starts a new batch rather than let the next item push
/// that sum over `budget` or that batch's own length past `max_count`.
/// This is the one packer behind two callers that both answer to the same
/// Discord rule, a message's real budget is a sum and a count across
/// everything on it, never a property of one item alone:
/// [`into_messages`] below, packing this module's own [`Chunk`]s onto a
/// log message, and [`crate::bot::commands::shep`]'s `/shep list`, packing
/// one embed per sheep onto an interaction followup. A second hand-rolled
/// walk here was already the wrong fix once, in the old code's own
/// ten-embed cap that counted embeds and never their characters; this
/// keeps there being only one.
#[must_use]
pub fn pack_by_budget<T>(
    items: Vec<T>,
    budget: usize,
    max_count: usize,
    size: impl Fn(&T) -> usize,
) -> Vec<Vec<T>> {
    let mut batches: Vec<Vec<T>> = Vec::new();
    let mut current_total = 0usize;

    for item in items {
        let item_size = size(&item);
        let current = batches.last();
        let fits_current = current
            .is_some_and(|batch| batch.len() < max_count && current_total + item_size <= budget);
        if fits_current {
            current_total += item_size;
        } else {
            batches.push(Vec::new());
            current_total = item_size;
        }
        batches.last_mut().expect("just pushed if empty").push(item);
    }

    batches
}

/// Group `chunks` into messages, each under [`MESSAGE_CHARACTER_BUDGET`].
///
/// [`pack_by_budget`] does the walking; this hands it the character count
/// [`chunks`] guarantees stays under budget for a single chunk (see below)
/// and Discord's own [`crate::limits::EMBED_MAX_COUNT`], the count cap a
/// log message runs into far less often than `/shep list` does, since a
/// busy sheep usually fills a message on characters alone long before it
/// reaches ten chunks. A single chunk can never exceed the budget on its
/// own: it is [`chunks`] that establishes this, by capping every title at
/// [`EMBED_TITLE_LIMIT`] and every description at
/// [`EMBED_DESCRIPTION_LIMIT`], so `256 + 4096 = 4352 < 6000` and every
/// chunk always finds room in a message of its own even in the degenerate
/// case where it shares one with nothing else.
#[must_use]
pub fn into_messages(chunks: Vec<Chunk>) -> Vec<Vec<Chunk>> {
    pack_by_budget(
        chunks,
        MESSAGE_CHARACTER_BUDGET,
        limits::EMBED_MAX_COUNT,
        |chunk| chunk.title.chars().count() + chunk.description.chars().count(),
    )
}

/// Strip ANSI CSI escape sequences from `text`.
///
/// A sheep's own logger commonly colors its output for a terminal, and
/// those escapes are meaningless (and visible as `<binary>` sequences) once
/// they land in a Discord embed. A small state machine over `ESC [ ... final
/// byte` is well under the line count that would justify a dependency for
/// something this narrow: this only strips CSI sequences (`ESC [`), which is
/// what every common colorizer emits, not the full range of ANSI escapes.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // Only a CSI sequence, `ESC [ ... final byte`, is recognized and
        // dropped. An escape not followed by `[` is not a CSI sequence
        // this module knows how to end, so it is passed through rather
        // than swallowed along with an arbitrary amount of the rest of the
        // line.
        if chars.peek() == Some(&'[') {
            chars.next();
            for parameter_or_final in chars.by_ref() {
                if parameter_or_final.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{
        Chunk, EMBED_DESCRIPTION_LIMIT, EMBED_TITLE_LIMIT, MESSAGE_CHARACTER_BUDGET, chunks,
        into_messages, strip_ansi,
    };
    use crate::stream::buffer::Group;

    fn group(name: &str, text: &str) -> Group {
        Group {
            name: name.to_owned(),
            at_ms: 0,
            text: text.to_owned(),
        }
    }

    /// The count Discord itself sums across every embed on one message:
    /// every chunk's `title.chars().count() + description.chars().count()`.
    /// More than one test below needs this sum, once per message.
    fn message_character_count(message: &[Chunk]) -> usize {
        message
            .iter()
            .map(|chunk| chunk.title.chars().count() + chunk.description.chars().count())
            .sum()
    }

    #[test]
    fn a_short_group_is_one_untitled_chunk() {
        let group_chunks = chunks(&group("web", "hello"));
        assert_eq!(group_chunks.len(), 1);
        assert_eq!(group_chunks[0].title, "web");
        assert_eq!(group_chunks[0].description, "hello");
    }

    #[test]
    fn a_long_group_splits_at_the_description_limit_and_numbers_itself() {
        let group_chunks = chunks(&group("web", &"x".repeat(EMBED_DESCRIPTION_LIMIT + 1)));
        assert_eq!(group_chunks.len(), 2);
        assert_eq!(group_chunks[0].title, "web (1/2)");
        assert_eq!(group_chunks[1].title, "web (2/2)");
        assert_eq!(
            group_chunks[0].description.chars().count(),
            EMBED_DESCRIPTION_LIMIT
        );
        assert_eq!(group_chunks[1].description.chars().count(), 1);
    }

    /// The whole point of this module. Two full chunks is 8,192 characters,
    /// and Discord's limit is the 6,000 sum across every embed on ONE message,
    /// not an embed count. The old code sent every pending embed in one call
    /// and took a 400 for it, then retried the same batch forever because it
    /// cleared the queue only on success.
    #[test]
    fn two_full_chunks_never_ride_on_one_message() {
        let group_chunks = chunks(&group("web", &"x".repeat(EMBED_DESCRIPTION_LIMIT * 2)));
        assert_eq!(group_chunks.len(), 2);
        let messages = into_messages(group_chunks);
        assert_eq!(messages.len(), 2, "each full chunk needs its own message");
        for message in &messages {
            let total = message_character_count(message);
            assert!(total <= MESSAGE_CHARACTER_BUDGET, "{total} over budget");
        }
    }

    #[test]
    fn small_chunks_share_a_message_until_the_budget_is_spent() {
        let small_chunks: Vec<Chunk> = (0..10)
            .map(|i| Chunk {
                title: format!("sheep{i}"),
                description: "x".repeat(1_000),
            })
            .collect();
        let messages = into_messages(small_chunks);
        assert!(messages.len() >= 2);
        for message in &messages {
            let total = message_character_count(message);
            assert!(total <= MESSAGE_CHARACTER_BUDGET, "{total} over budget");
        }
        assert_eq!(
            messages.iter().map(Vec::len).sum::<usize>(),
            10,
            "no chunk is lost"
        );
    }

    #[test]
    fn the_budget_counts_characters_not_bytes() {
        // Discord counts characters. A description of 4,096 multibyte
        // characters is legal and roughly 12 KiB on the wire, so a byte-based
        // split would refuse a message Discord accepts and, worse, a
        // byte-based limit check would let an over-long one through.
        let group_chunks = chunks(&group("web", &"é".repeat(EMBED_DESCRIPTION_LIMIT)));
        assert_eq!(
            group_chunks.len(),
            1,
            "4,096 characters is one chunk however many bytes it is"
        );
    }

    #[test]
    fn ansi_escapes_do_not_reach_discord() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    /// A name past `EMBED_TITLE_LIMIT` is the input class the old doc
    /// comment's unenforced assumption missed: nothing in shep caps a
    /// sheep's name, so a title built straight from one could already
    /// exceed the limit on its own before this fix.
    #[test]
    fn a_name_longer_than_the_title_limit_is_truncated_to_it() {
        let long_name = "s".repeat(EMBED_TITLE_LIMIT * 2);
        let group_chunks = chunks(&group(&long_name, "hello"));
        assert_eq!(group_chunks.len(), 1);
        assert_eq!(group_chunks[0].title.chars().count(), EMBED_TITLE_LIMIT);
        assert!(group_chunks[0].title.ends_with('\u{2026}'));
    }

    /// A long name that also splits into several chunks still needs its
    /// `" (i/n)"` suffix on every one: that suffix is the only thing that
    /// tells an operator a message was split, so it is what a truncation
    /// has to preserve rather than cut.
    #[test]
    fn a_long_name_across_several_chunks_keeps_its_suffix_on_every_title() {
        let long_name = "s".repeat(EMBED_TITLE_LIMIT * 2);
        let group_chunks = chunks(&group(&long_name, &"x".repeat(EMBED_DESCRIPTION_LIMIT + 1)));
        assert_eq!(group_chunks.len(), 2);
        assert!(group_chunks[0].title.ends_with(" (1/2)"));
        assert!(group_chunks[1].title.ends_with(" (2/2)"));
        for chunk in &group_chunks {
            assert_eq!(chunk.title.chars().count(), EMBED_TITLE_LIMIT);
        }
    }

    /// The invariant [`into_messages`]'s doc comment claims: every message
    /// it returns fits Discord's real budget, even for the worst input this
    /// module can be handed, a long name and a long text together.
    #[test]
    fn a_long_name_and_a_long_text_still_produce_messages_under_budget() {
        let long_name = "s".repeat(EMBED_TITLE_LIMIT * 2);
        let group_chunks = chunks(&group(&long_name, &"x".repeat(EMBED_DESCRIPTION_LIMIT * 3)));
        let messages = into_messages(group_chunks);
        assert!(!messages.is_empty());
        for message in &messages {
            let total = message_character_count(message);
            assert!(total <= MESSAGE_CHARACTER_BUDGET, "{total} over budget");
            for chunk in message {
                assert!(
                    chunk.title.chars().count() <= EMBED_TITLE_LIMIT,
                    "{} over the title limit",
                    chunk.title
                );
            }
        }
    }
}
