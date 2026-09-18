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

/// The longest an embed's `description` may be.
pub const EMBED_DESCRIPTION_LIMIT: usize = 4096;

/// The character budget for everything counted, summed across every embed
/// on one message: every `title`, `description`, `field.name`,
/// `field.value`, `footer.text` and `author.name`. This module only ever
/// builds a `title` and a `description`, so [`into_messages`] only ever
/// sums those two.
pub const MESSAGE_CHARACTER_BUDGET: usize = 6000;

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

/// Split `group`'s text into one or more [`Chunk`]s, none longer than
/// [`EMBED_DESCRIPTION_LIMIT`] characters.
///
/// Splits on character boundaries, not byte boundaries: Discord counts
/// characters, so a byte based split could cut a multibyte character in
/// half, or refuse a description this limit actually allows. A group that
/// fits in one chunk gets a bare title; a group that needs more than one
/// gets `"<name> (i/n)"` on each, so an operator reading a busy channel can
/// tell a split message from an unrelated one under the same sheep.
#[must_use]
#[allow(
    dead_code,
    reason = "called by the stream-driving task once a Group comes off Buffer::drain, not yet written"
)]
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
            let title = if total > 1 {
                format!("{} ({}/{total})", group.name, index + 1)
            } else {
                group.name.clone()
            };
            Chunk { title, description }
        })
        .collect()
}

/// Group `chunks` into messages, each under [`MESSAGE_CHARACTER_BUDGET`].
///
/// Walks the chunks in order, keeping a running sum of
/// `title.chars().count() + description.chars().count()` for the message
/// being built, and starts a new message rather than let the next chunk
/// push that sum over budget. A single chunk can never exceed the budget on
/// its own, since a chunk's title is at most a sheep's name plus a short
/// `" (i/n)"` suffix and its description is at most
/// [`EMBED_DESCRIPTION_LIMIT`], and Discord's own 256 character title
/// limit satisfies `4096 + 256 < 6000`, so every chunk always finds room in
/// a message of its own even in the degenerate case where it shares one
/// with nothing else.
#[must_use]
#[allow(
    dead_code,
    reason = "called by the stream-driving task before it hands messages to the Discord client, not yet written"
)]
pub fn into_messages(chunks: Vec<Chunk>) -> Vec<Vec<Chunk>> {
    let mut messages: Vec<Vec<Chunk>> = Vec::new();
    let mut current_total = 0usize;

    for chunk in chunks {
        let chunk_size = chunk.title.chars().count() + chunk.description.chars().count();
        let fits_current =
            messages.last().is_some() && current_total + chunk_size <= MESSAGE_CHARACTER_BUDGET;
        if fits_current {
            current_total += chunk_size;
        } else {
            messages.push(Vec::new());
            current_total = chunk_size;
        }
        messages
            .last_mut()
            .expect("just pushed if empty")
            .push(chunk);
    }

    messages
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
#[allow(
    dead_code,
    reason = "called wherever a raw bus line becomes a stream::buffer::Line, not yet written"
)]
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
        Chunk, EMBED_DESCRIPTION_LIMIT, MESSAGE_CHARACTER_BUDGET, chunks, into_messages, strip_ansi,
    };
    use crate::stream::buffer::Group;

    fn group(name: &str, text: &str) -> Group {
        Group {
            name: name.to_owned(),
            at_ms: 0,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_short_group_is_one_untitled_chunk() {
        let chunks = chunks(&group("web", "hello"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].title, "web");
        assert_eq!(chunks[0].description, "hello");
    }

    #[test]
    fn a_long_group_splits_at_the_description_limit_and_numbers_itself() {
        let chunks = chunks(&group("web", &"x".repeat(EMBED_DESCRIPTION_LIMIT + 1)));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].title, "web (1/2)");
        assert_eq!(chunks[1].title, "web (2/2)");
        assert_eq!(
            chunks[0].description.chars().count(),
            EMBED_DESCRIPTION_LIMIT
        );
        assert_eq!(chunks[1].description.chars().count(), 1);
    }

    /// The whole point of this module. Two full chunks is 8,192 characters,
    /// and Discord's limit is the 6,000 sum across every embed on ONE message,
    /// not an embed count. The old code sent every pending embed in one call
    /// and took a 400 for it, then retried the same batch forever because it
    /// cleared the queue only on success.
    #[test]
    fn two_full_chunks_never_ride_on_one_message() {
        let chunks = chunks(&group("web", &"x".repeat(EMBED_DESCRIPTION_LIMIT * 2)));
        assert_eq!(chunks.len(), 2);
        let messages = into_messages(chunks);
        assert_eq!(messages.len(), 2, "each full chunk needs its own message");
        for message in &messages {
            let total: usize = message
                .iter()
                .map(|chunk| chunk.title.chars().count() + chunk.description.chars().count())
                .sum();
            assert!(total <= MESSAGE_CHARACTER_BUDGET, "{total} over budget");
        }
    }

    #[test]
    fn small_chunks_share_a_message_until_the_budget_is_spent() {
        let chunks: Vec<Chunk> = (0..10)
            .map(|i| Chunk {
                title: format!("sheep{i}"),
                description: "x".repeat(1_000),
            })
            .collect();
        let messages = into_messages(chunks);
        assert!(messages.len() >= 2);
        for message in &messages {
            let total: usize = message
                .iter()
                .map(|chunk| chunk.title.chars().count() + chunk.description.chars().count())
                .sum();
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
        let chunks = chunks(&group("web", &"é".repeat(EMBED_DESCRIPTION_LIMIT)));
        assert_eq!(
            chunks.len(),
            1,
            "4,096 characters is one chunk however many bytes it is"
        );
    }

    #[test]
    fn ansi_escapes_do_not_reach_discord() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    }
}
