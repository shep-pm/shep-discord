//! The one [`Sink`] that actually reaches Discord.
//!
//! Log streaming needs Discord's REST API and nothing else: no gateway, no
//! shard, no event loop. `serenity::http::Http::new` (`http/client.rs:208`
//! in the vendored 0.12.5 source, itself calling `HttpBuilder::new` at
//! `:67`) builds a client that opens no gateway connection, and `impl
//! CacheHttp for Http` (`http/mod.rs:118`, alongside the blanket `impl<T>
//! CacheHttp for &T` just above it) is what makes `&Http` acceptable
//! wherever [`serenity::model::id::ChannelId::send_message`] asks for
//! `impl CacheHttp`. Nothing here builds a [`serenity::Client`] or opens a
//! gateway; the slash command side of this dog does that, in
//! [`crate::bot::run`].
//!
//! # Why there is a second trait here
//!
//! [`Sink`] is already a seam, and it is the wrong one for this file.
//! It is what puts [`crate::stream::state::State`] behind a fake, and a
//! fake standing in for [`DiscordSink`] proves nothing ABOUT
//! [`DiscordSink`]: the whole of what this module does, turning a batch of
//! [`Chunk`]s into one Discord message and posting it, sits on the far
//! side of it. This file used to hold one test that constructed a sink
//! and one that read its `Debug`, and the largest untested surface in the
//! crate in between.
//!
//! [`Rest`] is the seam one layer down, and it is
//! [`crate::bot::channel::Board`] applied to the log channel instead of
//! the monitor one: a trait over the single REST route this module needs,
//! implemented for real by serenity's own [`Http`] and by a recording
//! fake in this module's tests. Everything on this side of it, which is
//! every decision [`DiscordSink`] makes, is then testable with no token,
//! no gateway and no channel, and the tests read what was sent rather
//! than arguing from serenity's source that nothing was.

use core::fmt;

use serenity::all::{ChannelId, CreateEmbed, CreateMessage, Http};

use crate::stream::{Sink, SinkError, pack::Chunk};

/// A marker whose `Debug` always prints the same placeholder, never the
/// value it stands in for.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted>")
    }
}

/// The one Discord REST route log streaming uses.
///
/// [`Http`] is the only implementation that reaches the network, and the
/// only thing in this module a test cannot exercise, exactly as [`Live`]
/// is for [`Board`]. The method is deliberately the whole route and
/// nothing more: it takes the message already built, so the building is
/// on the testable side of the seam and a fake has the same body a real
/// request would have carried.
///
/// The error is serenity's own rather than [`SinkError`], which matters
/// more than it looks. Collapsing every failure onto the one [`SinkError`]
/// variant is a decision [`DiscordSink::send`] makes, and the reason for
/// pushing the seam below it is that a seam above it would put that
/// decision back on the untestable side, which is the gap this trait
/// exists to close. See [`SinkError`] for why there is only the one
/// variant to collapse onto, and why this crate must never grow a second
/// one for a rate limit.
///
/// `async fn` in a trait rather than a boxed future, and no `Send` bound
/// spelled on the returned future, for the same reason [`Sink`] carries
/// neither: this is used through a generic bound only, never behind
/// `dyn`, and nothing spawns a future made from one.
/// [`Board`] does spell `Send`, because
/// [`crate::stream::run`] hands its redraws to `tokio::spawn`; nothing
/// spawns a log send.
///
/// [`Live`]: crate::bot::channel::Live
/// [`Board`]: crate::bot::channel::Board
pub trait Rest {
    /// Post `message` to `channel`.
    ///
    /// The `Message` Discord answers with is dropped rather than returned.
    /// Nothing in log streaming edits or deletes what it sent, so unlike
    /// [`crate::bot::channel::Board::post`], which hands back the
    /// [`serenity::all::MessageId`] a later edit needs, there is no id
    /// here for a caller to want.
    ///
    /// # Errors
    /// [`serenity::Error`] as serenity reports it, unmapped: a shape
    /// Discord refused, a transport failure, anything else it can return.
    async fn send_message(
        &self,
        channel: ChannelId,
        message: CreateMessage,
    ) -> Result<(), serenity::Error>;
}

impl Rest for Http {
    async fn send_message(
        &self,
        channel: ChannelId,
        message: CreateMessage,
    ) -> Result<(), serenity::Error> {
        channel.send_message(self, message).await.map(|_message| ())
    }
}

/// A [`Sink`] backed by Discord's HTTP API.
///
/// Generic over its transport, and built as `DiscordSink<Http>` everywhere
/// outside this module's own tests. Holds an [`Http`] rather than a full
/// [`serenity::Client`]: sending a message needs a bot token and a route,
/// and both live on [`Http`] alone, so this type never opens the gateway
/// connection a `Client` would.
///
/// `Debug` is hand-written rather than derived, the same reason
/// [`crate::config::Config`]'s is. `Http` holds the bot token this dog was
/// built with behind `secrecy::SecretString`, whose own `Debug` already
/// redacts it (nothing in this crate proves that; it is serenity's
/// contract, not this crate's), so nothing leaks today. But that safety
/// sits in a dependency this crate does not test, and a derived `Debug`
/// here would put whatever `Http`'s own `Debug` prints into any log line or
/// panic message that renders a `DiscordSink` in an error chain. Pinned by
/// `the_token_never_reaches_a_debug_line`, because a later
/// `#[derive(Debug)]` here would be a silent regression. Written for every
/// `R` rather than for `Http` alone, so a transport added later is
/// redacted without anyone having to remember to redact it.
pub struct DiscordSink<R> {
    rest: R,
}

impl<R> fmt::Debug for DiscordSink<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordSink")
            .field("rest", &Redacted)
            .finish()
    }
}

impl<R: Rest> DiscordSink<R> {
    /// A sink sending through `rest`.
    fn over(rest: R) -> Self {
        Self { rest }
    }
}

impl DiscordSink<Http> {
    /// Build a sink authenticated as the bot `token` names.
    ///
    /// `Http::new` never makes a network call of its own; the token is
    /// only checked against Discord the first time [`Sink::send`] uses it.
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self::over(Http::new(token))
    }
}

impl<R: Rest> Sink for DiscordSink<R> {
    /// Send one message per call, one embed per [`Chunk`], `title` and
    /// `description` copied straight across: [`crate::stream::pack::chunks`]
    /// and [`crate::stream::pack::into_messages`] already fit both to
    /// Discord's own limits, so nothing here re-derives that budget.
    ///
    /// Every failure [`Rest::send_message`] can return, a rejected shape or
    /// anything else, collapses to [`SinkError::BadRequest`]: see
    /// [`SinkError`]'s own doc for why a rate limit never reaches this far,
    /// and why there is only the one variant left to map onto.
    ///
    /// An empty `chunks` is refused before the request rather than by it.
    /// Discord rejects a message carrying no content, no embed and no
    /// attachment, so the answer is the same either way and this is the
    /// cheaper place to get it. What the guard is really for is the
    /// caller: [`crate::stream::state::State`] clears what it flushed
    /// once this returns `Ok`, so a batch that arrived here empty when it
    /// should have held lines would take them with it silently. Nothing
    /// produces one today, and `an_empty_batch_is_refused_without_a_round_trip`
    /// says why that is worth a guard anyway.
    async fn send(&self, channel: u64, chunks: Vec<Chunk>) -> Result<(), SinkError> {
        if chunks.is_empty() {
            return Err(SinkError::BadRequest);
        }
        let mut message = CreateMessage::new();
        for chunk in chunks {
            message = message.add_embed(
                CreateEmbed::new()
                    .title(chunk.title)
                    .description(chunk.description),
            );
        }
        self.rest
            .send_message(ChannelId::new(channel), message)
            .await
            .map_err(|_err| SinkError::BadRequest)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serenity::{
        all::{ChannelId, CreateMessage},
        model::ModelError,
    };

    use crate::{
        limits::{EMBED_DESCRIPTION_LIMIT, EMBED_MAX_COUNT, EMBED_TITLE_LIMIT},
        stream::{Sink, SinkError, pack::Chunk},
    };

    use super::{DiscordSink, Rest};

    /// A [`Rest`] that records every message it is handed and reaches no
    /// network at all, or refuses the one call a test armed it to refuse.
    ///
    /// The log sink's counterpart to
    /// [`crate::test_support::CountingChannel`], and private to this
    /// module rather than shared from `test_support` for the same reason
    /// [`crate::stream::state`]'s own `Recording` is: one module drives
    /// it. `CountingChannel` is shared because two do.
    #[derive(Default)]
    struct Recording {
        sent: Mutex<Vec<(ChannelId, CreateMessage)>>,
        attempts: Mutex<usize>,
        refuse: Mutex<Option<serenity::Error>>,
    }

    impl Recording {
        fn new() -> Self {
            Self::default()
        }

        /// A fake whose next call fails with `error`. Every test built on
        /// one makes exactly one call and asserts the attempt count, so a
        /// second call quietly succeeding is never what a test reads.
        fn refusing(error: serenity::Error) -> Self {
            Self {
                refuse: Mutex::new(Some(error)),
                ..Self::default()
            }
        }

        /// Every message sent, as the JSON serenity would have put on the
        /// wire, paired with the channel it was addressed to.
        ///
        /// JSON rather than the builder's own fields: `CreateMessage`
        /// keeps them private, and its `Serialize` is the same rendering
        /// the real request body gets, so a test reading it is reading
        /// what Discord would have been sent. [`crate::bot::embed`]'s own
        /// tests measure an embed the same way.
        fn sent(&self) -> Vec<(ChannelId, serde_json::Value)> {
            self.sent
                .lock()
                .expect("not poisoned")
                .iter()
                .map(|(channel, message)| (*channel, serde_json::to_value(message).expect("json")))
                .collect()
        }

        /// How many times the sink reached for the network, successful or
        /// not, so a refusal test can tell a call that failed apart from a
        /// call that never happened.
        fn attempts(&self) -> usize {
            *self.attempts.lock().expect("not poisoned")
        }
    }

    impl Rest for Recording {
        async fn send_message(
            &self,
            channel: ChannelId,
            message: CreateMessage,
        ) -> Result<(), serenity::Error> {
            *self.attempts.lock().expect("not poisoned") += 1;
            if let Some(error) = self.refuse.lock().expect("not poisoned").take() {
                return Err(error);
            }
            self.sent
                .lock()
                .expect("not poisoned")
                .push((channel, message));
            Ok(())
        }
    }

    /// One chunk, its title and description distinct enough that a test
    /// reading the JSON back can tell which chunk it came from and which
    /// field it landed in.
    fn chunk(n: usize) -> Chunk {
        Chunk {
            title: format!("title-{n}"),
            description: format!("description-{n}"),
        }
    }

    /// The embeds on the one message sent, as JSON.
    fn embeds_of(rest: &Recording) -> Vec<serde_json::Value> {
        let sent = rest.sent();
        assert_eq!(sent.len(), 1, "one call sends one message");
        sent[0].1["embeds"]
            .as_array()
            .expect("embeds is an array")
            .clone()
    }

    /// A batch is one Discord message carrying one embed per chunk, not
    /// one message per chunk and not one embed replacing the last.
    /// [`crate::stream::pack::into_messages`] already fitted the batch to
    /// Discord's budget, so a message boundary drawn a second time here
    /// would split a batch it deliberately kept whole and spend an HTTP
    /// round trip on each piece.
    ///
    /// A full [`EMBED_MAX_COUNT`] of chunks, because the count is one of
    /// the two halves of that budget and a cap reapplied here is what a
    /// smaller batch would not show.
    /// `a_batch_reaches_discord_exactly_as_it_was_packed` is the same
    /// claim on the other half, the character sum.
    #[tokio::test]
    async fn a_batch_at_the_embed_count_cap_is_still_one_message() {
        let rest = Recording::new();
        let sink = DiscordSink::over(rest);

        sink.send(7, (1..=EMBED_MAX_COUNT).map(chunk).collect())
            .await
            .expect("sent");

        assert_eq!(sink.rest.sent().len(), 1, "one message, not one per chunk");
        assert_eq!(sink.rest.attempts(), 1);
        assert_eq!(embeds_of(&sink.rest).len(), EMBED_MAX_COUNT);
    }

    /// Title to title and description to description, in the order
    /// [`crate::stream::pack::chunks`] produced them. A log tail read out
    /// of order is not a log tail, and a title and a description swapped
    /// would put four thousand characters of output into a field Discord
    /// caps at 256.
    #[tokio::test]
    async fn each_embed_carries_its_own_chunks_title_and_description() {
        let rest = Recording::new();
        let sink = DiscordSink::over(rest);

        sink.send(7, vec![chunk(1), chunk(2), chunk(3)])
            .await
            .expect("sent");

        let embeds = embeds_of(&sink.rest);
        for (at, embed) in embeds.iter().enumerate() {
            let n = at + 1;
            assert_eq!(embed["title"], format!("title-{n}"));
            assert_eq!(embed["description"], format!("description-{n}"));
        }
    }

    /// The channel comes from the call, not from the sink. One
    /// `DiscordSink` serves the whole flock and every sheep may name a
    /// channel of its own, so a sink that remembered the first channel it
    /// was given would put every sheep's output in one sheep's channel.
    #[tokio::test]
    async fn a_batch_goes_to_the_channel_the_caller_named() {
        let rest = Recording::new();
        let sink = DiscordSink::over(rest);

        sink.send(11, vec![chunk(1)]).await.expect("sent");
        sink.send(22, vec![chunk(2)]).await.expect("sent");

        let channels: Vec<ChannelId> = sink
            .rest
            .sent()
            .into_iter()
            .map(|(channel, _message)| channel)
            .collect();
        assert_eq!(channels, vec![ChannelId::new(11), ChannelId::new(22)]);
    }

    /// Every way Discord can refuse becomes [`SinkError::BadRequest`],
    /// and a refusal is never reported as a success.
    ///
    /// Reporting one would be worse than the refusal itself:
    /// [`crate::stream::state::State`] clears what it flushed once the
    /// sink says it went out, so a swallowed error is a tail of log lines
    /// nobody ever sees and nobody is told about.
    #[tokio::test]
    async fn every_refusal_is_reported_as_a_bad_request() {
        for error in [
            serenity::Error::Other("expected value"),
            serenity::Error::Model(ModelError::MessageTooLong(20)),
            serenity::Error::Url("not a url".to_owned()),
        ] {
            let sink = DiscordSink::over(Recording::refusing(error));

            let outcome = sink.send(7, vec![chunk(1)]).await;

            assert_eq!(outcome, Err(SinkError::BadRequest));
            assert_eq!(sink.rest.attempts(), 1, "it tried, and was refused");
        }
    }

    /// An empty batch is refused here, loudly, instead of at Discord.
    ///
    /// [`crate::stream::pack::into_messages`] does not produce one today:
    /// its `pack_by_budget` opens a batch only when it has an item to put
    /// in it, so no chunks come back as no messages rather than as one
    /// empty message. This is the guard for the day that stops being
    /// true. Discord refuses a message carrying no content, no embed and
    /// no attachment, so the round trip was only ever going to buy the
    /// same answer a request later.
    ///
    /// Refused rather than quietly accepted, which is the tempting shape
    /// and the wrong one: [`crate::stream::state::State`] clears what it
    /// flushed once the sink says it went out, so a batch that arrived
    /// here empty when it should have held lines would take those lines
    /// with it and say nothing. `a_rejected_batch_is_dropped_not_retried`
    /// covers what the caller does with the refusal.
    #[tokio::test]
    async fn an_empty_batch_is_refused_without_a_round_trip() {
        let rest = Recording::new();
        let sink = DiscordSink::over(rest);

        let outcome = sink.send(7, Vec::new()).await;

        assert_eq!(outcome, Err(SinkError::BadRequest));
        assert_eq!(sink.rest.attempts(), 0, "nothing was sent to be refused");
    }

    /// A batch reaches Discord exactly as [`crate::stream::pack`] built
    /// it: nothing truncated, nothing re-split, however wide the chunks
    /// are.
    ///
    /// Two chunks at both of Discord's per-embed caps sum to 8,704
    /// characters, over the 6,000 a single message is allowed, which is a
    /// shape `pack` would never hand over. It is handed over here on
    /// purpose, because a budget re-derived at this layer is the failure
    /// this crate has already made five times, and the only way to see
    /// one is to send something that would trip it.
    #[tokio::test]
    async fn a_batch_reaches_discord_exactly_as_it_was_packed() {
        let wide = Chunk {
            title: "t".repeat(EMBED_TITLE_LIMIT),
            description: "d".repeat(EMBED_DESCRIPTION_LIMIT),
        };
        let rest = Recording::new();
        let sink = DiscordSink::over(rest);

        sink.send(7, vec![wide.clone(), wide.clone()])
            .await
            .expect("sent");

        let embeds = embeds_of(&sink.rest);
        assert_eq!(embeds.len(), 2, "not re-split, and not dropped");
        for embed in &embeds {
            assert_eq!(embed["title"], wide.title);
            assert_eq!(embed["description"], wide.description);
        }
    }

    /// `DiscordSink`'s `Debug` is hand-written, and this is what pins it.
    /// A later `#[derive(Debug)]` here would print whatever the transport's
    /// own `Debug` prints, which is not this crate's call to make. Today
    /// that transport is serenity's `Http`, which holds the token in a
    /// `secrecy::SecretString` that redacts itself, so a derive would not
    /// spill it. That is a dependency's behaviour no test here versions,
    /// and it covers one `R`: `DiscordSink` is generic, so a transport
    /// added later carries no such guarantee and a fake in these tests
    /// holds its token in plain sight. An exact string, not a `contains`:
    /// a redaction that stops covering a newly added field still passes a
    /// `contains` check.
    #[test]
    fn the_token_never_reaches_a_debug_line() {
        let sink = DiscordSink::new("MTIzNDU2Nzg5.GaBcDe.ThisIsNotARealToken");
        let rendered = format!("{sink:?}");
        assert_eq!(rendered, "DiscordSink { rest: <redacted> }");
        assert!(!rendered.contains("ThisIsNotARealToken"), "{rendered}");
    }
}
