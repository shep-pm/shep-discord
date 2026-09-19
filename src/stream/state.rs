//! Buffering and filtering one bus subscription into per-channel groups.
//!
//! Split out of `stream/mod.rs` for the same reason [`crate::stream::discord`]
//! already is: production code in that file was over this crate's 500-line
//! ceiling, and [`State`] is a whole pipeline stage in its own right, worth
//! reading (and testing, with no client, no socket, and no running bot
//! behind it) apart from the trait and the loop that drive it.

use std::time::{SystemTime, UNIX_EPOCH};

use shep_client::{
    Lagged,
    shep_core::protocol::{BusEvent, ProcessInfo},
};

use crate::{
    config::Config,
    names::Names,
    stream::{
        Sink, SinkError,
        buffer::{Buffer, Line},
        pack::{self, Chunk},
    },
};

/// The name a bus-level notice renders under.
///
/// A `Dropped` or a `Lagged` item carries a count, not a sheep id: neither
/// is about any one sheep, so there is no name from [`Names`] to render it
/// under instead.
const BUS_NOTICE_NAME: &str = "shep-discord";

/// The sentence a dropped-line count renders as, whether the daemon's own
/// bus reported it (`BusEvent::Dropped`), this stream's local subscription
/// fell behind (`Lagged`), or this dog's own [`Buffer`] dropped a line for
/// capacity. An operator reading the channel does not need to know which of
/// the three lost the lines, only that some were.
fn dropped_notice(count: u64) -> String {
    format!("{count} lines dropped: this dog fell behind the shepherd's bus")
}

/// Milliseconds since the Unix epoch, for a [`Line::at_ms`] built from a
/// live event rather than a test's own fixed clock.
///
/// [`Line::at_ms`] is only ever compared to another `Line` from the same
/// [`Buffer`], so the epoch does not matter; wall-clock time is used
/// because it is already at hand and orders lines the way they actually
/// arrived. `unwrap_or_default` rather than a panic: a clock set before
/// 1970 is not a reason for this dog to stop streaming.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One buffered stream: the channel it posts to, and the lines it is
/// holding.
struct Sided {
    channel: u64,
    buffer: Buffer,
}

/// The state one bus subscription drives: two buffers, one per stream, and
/// the name cache lines are rendered through.
///
/// Two buffers rather than one, so stdout and stderr keep separate windows
/// and separate channels: a burst on one stream never delays or reorders
/// the other's, and a group never mixes lines from both. Either side is
/// simply absent when its channel is unset, per [`Self::from_config`],
/// rather than present and discarded: an operator who never configured
/// `err_channel` gets no buffer spent on it at all.
pub struct State<'a, S: Sink> {
    own_id: Option<u32>,
    names: Names,
    out: Option<Sided>,
    err: Option<Sided>,
    coalesce_ms: u64,
    sink: &'a S,
}

impl<'a, S: Sink> State<'a, S> {
    /// A state with both streams enabled on channel `0`, for a test that
    /// does not care which channel a line lands on.
    #[must_use]
    #[allow(
        dead_code,
        reason = "called only by this module's own tests; run builds a State through from_config"
    )]
    pub fn new(own_id: Option<u32>, sink: &'a S) -> Self {
        Self::with_channels(own_id, Some(0), Some(0), sink)
    }

    /// A state with each stream's channel chosen explicitly, `None`
    /// disabling that stream. Buffer sizing and coalescing use this crate's
    /// own defaults; a caller wiring a real [`Config`] wants
    /// [`Self::from_config`] instead.
    #[must_use]
    #[allow(
        dead_code,
        reason = "called only by this module's own tests; run builds a State through from_config"
    )]
    pub fn with_channels(
        own_id: Option<u32>,
        log_channel: Option<u64>,
        err_channel: Option<u64>,
        sink: &'a S,
    ) -> Self {
        Self::from_parts(
            own_id,
            Names::new(),
            log_channel,
            err_channel,
            crate::config::DEFAULT_BUFFER_LINES,
            crate::config::DEFAULT_COALESCE_MS,
            sink,
        )
    }

    /// A state built from an already-parsed `[discord]` section, with
    /// `names` seeded from whatever the caller's muster-roll read already
    /// found.
    #[must_use]
    pub fn from_config(own_id: Option<u32>, names: Names, config: &Config, sink: &'a S) -> Self {
        Self::from_parts(
            own_id,
            names,
            config.log_channel,
            config.err_channel,
            config.buffer_lines,
            config.coalesce.as_millis(),
            sink,
        )
    }

    fn from_parts(
        own_id: Option<u32>,
        names: Names,
        log_channel: Option<u64>,
        err_channel: Option<u64>,
        buffer_lines: usize,
        coalesce_ms: u64,
        sink: &'a S,
    ) -> Self {
        Self {
            own_id,
            names,
            out: log_channel.map(|channel| Sided {
                channel,
                buffer: Buffer::new(buffer_lines),
            }),
            err: err_channel.map(|channel| Sided {
                channel,
                buffer: Buffer::new(buffer_lines),
            }),
            coalesce_ms,
            sink,
        }
    }

    /// Replace the id-to-name cache wholesale, the same replace-not-merge
    /// contract [`Names::refresh`] documents.
    ///
    /// Called whenever a `process.start` or `process.delete` bus event
    /// arrives: neither is handled by [`Self::on_event`], because answering
    /// one means a fresh `Live::flock` round trip, and this type does no
    /// I/O of its own so it can stay driven by a plain, synchronous test.
    pub fn refresh_names(&mut self, roll: &[ProcessInfo]) {
        self.names.refresh(roll);
    }

    /// Fold one bus event into whichever buffer it belongs to, if any.
    ///
    /// A `LogOut`/`LogErr` whose id is this dog's own is discarded before it
    /// reaches a buffer: the old code this project ports compared against a
    /// literal package name (`Discord.ts:73`), so a renamed install logged
    /// its own output into the channel it was writing to, forever. `own_id`
    /// is a number the shepherd assigned this dog, so a rename of the
    /// display name that number is resolved from cannot break the filter
    /// the way a hardcoded string did.
    ///
    /// A `Dropped` event is not an error path: the daemon's bus ring is
    /// bounded at 1,024 events, and this is how it tells a slow subscriber
    /// it fell behind. It becomes a line an operator can actually see,
    /// through [`dropped_notice`], never a silent hole in the channel.
    ///
    /// `process.start`, `process.delete`, a channel message, and a daemon
    /// shutdown notice all fall through untouched: see [`Self::refresh_names`]
    /// for the first two, and [`crate::stream::run`] for why the other two
    /// need nothing from this type at all.
    pub fn on_event(&mut self, event: BusEvent) {
        match event {
            BusEvent::LogOut { id, line } => self.push(Side::Out, id, line),
            BusEvent::LogErr { id, line } => self.push(Side::Err, id, line),
            BusEvent::Dropped { count } => self.push_notice(count),
            _ => {}
        }
    }

    /// A [`Lagged`] item from the local subscription itself, not the
    /// daemon's bus: see [`Lagged`]'s own doc for how the two differ.
    /// Rendered through the same [`dropped_notice`] text, because an
    /// operator reading the channel has no use for telling the two causes
    /// apart.
    pub fn on_lagged(&mut self, lagged: Lagged) {
        self.push_notice(lagged.count);
    }

    /// Resolve `id` to a name and push it onto the side named by `side`,
    /// unless `id` is this dog's own or that side has no channel configured.
    ///
    /// `own_id` is `None` for a dog nobody adopted: shep captures none of
    /// an unadopted process's output, so there is no id of its own that
    /// could ever show up on the bus to filter.
    fn push(&mut self, side: Side, id: u32, line: String) {
        if self.own_id == Some(id) {
            return;
        }
        let name = self.names.get(id);
        let Some(sided) = self.side_mut(side) else {
            return;
        };
        sided.buffer.push(Line {
            at_ms: now_ms(),
            name,
            // Sheep commonly colour their own output for a terminal; those
            // escapes are meaningless, and visible as raw bytes, once they
            // land in a Discord embed.
            text: pack::strip_ansi(&line),
        });
    }

    /// Push a synthetic dropped-line notice, preferring the stdout buffer
    /// and falling back to stderr when only that one is configured. The
    /// notice is about the bus falling behind, not about either stream in
    /// particular, so either buffer is as good a home for it as the other.
    fn push_notice(&mut self, count: u64) {
        let sided = if self.out.is_some() {
            self.out.as_mut()
        } else {
            self.err.as_mut()
        };
        let Some(sided) = sided else {
            return;
        };
        sided.buffer.push(dropped_line(count));
    }

    fn side_mut(&mut self, side: Side) -> Option<&mut Sided> {
        match side {
            Side::Out => self.out.as_mut(),
            Side::Err => self.err.as_mut(),
        }
    }

    /// Drain both buffers, pack each into Discord messages, and hand every
    /// message to the sink, one at a time.
    ///
    /// A capacity drop counted by [`Buffer::take_dropped`] is folded into
    /// the same [`dropped_notice`] text a bus-level `Dropped` renders as,
    /// and pushed before that side drains, so it reports alongside whatever
    /// else this flush is about to send rather than waiting for the next
    /// one.
    ///
    /// A failed [`Sink::send`] drops its batch rather than retrying it: see
    /// [`SinkError`]'s own doc for why retrying is worse than losing it, and
    /// for why a rate limit never reaches this far in the first place.
    pub async fn flush(&mut self) {
        Self::flush_side(&mut self.out, self.coalesce_ms, self.sink).await;
        Self::flush_side(&mut self.err, self.coalesce_ms, self.sink).await;
    }

    async fn flush_side(sided: &mut Option<Sided>, coalesce_ms: u64, sink: &S) {
        let Some(sided) = sided else { return };
        let dropped = sided.buffer.take_dropped();
        if dropped > 0 {
            sided.buffer.push(dropped_line(dropped));
        }
        for group in sided.buffer.drain(coalesce_ms) {
            let chunks = pack::chunks(&group);
            for message in pack::into_messages(chunks) {
                send_or_drop(sink, sided.channel, message).await;
            }
        }
    }
}

/// Which buffered stream a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Out,
    Err,
}

/// A [`Line`] carrying a dropped-count notice, built once here rather than
/// separately in [`State::push_notice`] and [`State::flush_side`]: both call
/// sites need the identical literal and neither can call the other, since
/// one borrows `self.out`/`self.err` and the other only ever sees one
/// `Sided` at a time.
fn dropped_line(count: u64) -> Line {
    Line {
        at_ms: now_ms(),
        name: BUS_NOTICE_NAME.to_owned(),
        text: dropped_notice(count),
    }
}

/// The character total Discord counts across one message: every chunk's
/// `title.chars().count() + description.chars().count()`, summed. Used only
/// to name the size of a batch [`SinkError::BadRequest`] refused, in the
/// message this dog prints for an operator to read.
fn message_characters(message: &[Chunk]) -> usize {
    message
        .iter()
        .map(|chunk| chunk.title.chars().count() + chunk.description.chars().count())
        .sum()
}

/// The message printed when a channel refuses a batch, naming its size so an
/// operator can tell an oversized batch from any other rejection.
///
/// A function rather than an inline `eprintln!`, so the dash check can reach
/// the text directly.
fn refused_message(channel: u64, characters: usize, err: SinkError) -> String {
    format!(
        "shep-discord: channel {channel} refused a {characters} character message, dropping \
         it: {err}"
    )
}

/// Send `message` to `channel`, dropping it and printing why on failure.
///
/// No retry: see [`SinkError`]'s own doc for why a rate limit never reaches
/// here, and [`State::flush`] for why a `BadRequest` is not retried either.
async fn send_or_drop<S: Sink>(sink: &S, channel: u64, message: Vec<Chunk>) {
    let characters = message_characters(&message);
    if let Err(err) = sink.send(channel, message).await {
        eprintln!("{}", refused_message(channel, characters, err));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Names the id being constructed, so a test reads `own_id(9)` rather
    /// than a bare `9` that could be mistaken for somebody else's.
    fn own_id(id: u32) -> Option<u32> {
        Some(id)
    }

    /// A [`Sink`] that either records every chunk it is handed, tagged with
    /// the channel it went to, or refuses every attempt with a fixed error.
    struct Recording {
        sent: Mutex<Vec<(u64, Chunk)>>,
        attempts: Mutex<usize>,
        reject: Option<SinkError>,
    }

    impl Recording {
        fn new() -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                attempts: Mutex::new(0),
                reject: None,
            }
        }

        fn rejecting(error: SinkError) -> Self {
            Self {
                reject: Some(error),
                ..Self::new()
            }
        }

        fn sent(&self) -> Vec<Chunk> {
            self.sent
                .lock()
                .expect("not poisoned")
                .iter()
                .map(|(_, chunk)| chunk.clone())
                .collect()
        }

        /// The channel a chunk whose description contains `text` went to,
        /// or `None` if nothing sent matches.
        fn channel_of(&self, text: &str) -> Option<u64> {
            self.sent
                .lock()
                .expect("not poisoned")
                .iter()
                .find(|(_, chunk)| chunk.description.contains(text))
                .map(|(channel, _)| *channel)
        }

        fn attempts(&self) -> usize {
            *self.attempts.lock().expect("not poisoned")
        }
    }

    impl Sink for Recording {
        async fn send(&self, channel: u64, chunks: Vec<Chunk>) -> Result<(), SinkError> {
            *self.attempts.lock().expect("not poisoned") += 1;
            if let Some(error) = self.reject {
                return Err(error);
            }
            let mut sent = self.sent.lock().expect("not poisoned");
            sent.extend(chunks.into_iter().map(|chunk| (channel, chunk)));
            Ok(())
        }
    }

    /// This dog's own output must not feed back into the channel it writes
    /// to. The old code compared against the literal package name at
    /// `Discord.ts:73`, so a renamed install logged its own logs.
    #[tokio::test]
    async fn the_dog_never_streams_its_own_output() {
        let sink = Recording::new();
        let mut state = State::new(own_id(9), &sink);
        state.on_event(BusEvent::LogOut {
            id: 9,
            line: "my own line".into(),
        });
        state.on_event(BusEvent::LogOut {
            id: 1,
            line: "web line".into(),
        });
        state.flush().await;
        let sent = sink.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].description.contains("web line"));
        assert!(!sent[0].description.contains("my own line"));
    }

    /// An unadopted dog has no id of its own on the bus at all: `own_id` is
    /// `None`, so nothing gets filtered, not even an id as extreme as
    /// `u32::MAX`. See `crate::session::resolve_own_id` for the case this
    /// pairs with, an adopted dog whose id has not resolved yet.
    #[tokio::test]
    async fn an_unadopted_dog_streams_everything_unfiltered() {
        let sink = Recording::new();
        let mut state = State::new(None, &sink);
        state.on_event(BusEvent::LogOut {
            id: u32::MAX,
            line: "web line".into(),
        });
        state.flush().await;
        let sent = sink.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].description.contains("web line"));
    }

    /// The daemon's bus ring is 1,024 events and it says so when a
    /// subscriber falls behind. Neither source repo could report a gap,
    /// because PM2's bus had no such signal.
    ///
    /// An exact match against `dropped_notice(37)`, not two independent
    /// substring checks: `dropped_notice` is deterministic, so two fragments
    /// each passing proves nothing about the whole sentence. A notice that
    /// said the opposite, e.g. "37 lines dropped: fixed now", would still
    /// pass both `contains` checks a substring assertion would make here.
    #[tokio::test]
    async fn a_dropped_run_is_reported_rather_than_hidden() {
        let sink = Recording::new();
        let mut state = State::new(own_id(9), &sink);
        state.on_event(BusEvent::Dropped { count: 37 });
        state.flush().await;
        let sent = sink.sent();
        assert_eq!(sent[0].description, dropped_notice(37));
    }

    /// A batch Discord refuses on its shape will be refused identically
    /// forever. Retrying it silences every line behind it, which is what
    /// the old code did at `Discord.ts:68`.
    #[tokio::test]
    async fn a_rejected_batch_is_dropped_not_retried() {
        let sink = Recording::rejecting(SinkError::BadRequest);
        let mut state = State::new(own_id(9), &sink);
        state.on_event(BusEvent::LogOut {
            id: 1,
            line: "web line".into(),
        });
        state.flush().await;
        state.flush().await;
        assert_eq!(
            sink.attempts(),
            1,
            "the second flush must not resend the refused batch"
        );
    }

    #[tokio::test]
    async fn stderr_and_stdout_go_to_their_own_channels() {
        let sink = Recording::new();
        let mut state = State::with_channels(own_id(9), Some(10), Some(20), &sink);
        state.on_event(BusEvent::LogOut {
            id: 1,
            line: "out".into(),
        });
        state.on_event(BusEvent::LogErr {
            id: 1,
            line: "err".into(),
        });
        state.flush().await;
        assert_eq!(sink.channel_of("out"), Some(10));
        assert_eq!(sink.channel_of("err"), Some(20));
    }

    /// A stream whose channel was never configured is never filled at all,
    /// rather than filled and then discarded at flush time: pushing a line
    /// there would grow a buffer nothing will ever drain.
    #[tokio::test]
    async fn an_unconfigured_channel_never_buffers_a_line() {
        let sink = Recording::new();
        let mut state = State::with_channels(own_id(9), Some(10), None, &sink);
        state.on_event(BusEvent::LogErr {
            id: 1,
            line: "err".into(),
        });
        state.flush().await;
        assert!(sink.sent().is_empty(), "no err_channel means no err buffer");
    }

    /// The local subscription falling behind is a different condition from
    /// the daemon's own bus dropping events, but an operator reading the
    /// channel gets one notice either way.
    ///
    /// An exact match against `dropped_notice(5)`, the same reasoning as
    /// `a_dropped_run_is_reported_rather_than_hidden`.
    #[tokio::test]
    async fn a_lagged_subscription_is_reported_the_same_way() {
        let sink = Recording::new();
        let mut state = State::new(own_id(9), &sink);
        state.on_lagged(Lagged { count: 5 });
        state.flush().await;
        let sent = sink.sent();
        assert_eq!(sent[0].description, dropped_notice(5));
    }

    /// New strings this task added, run through the same dash check every
    /// other person-facing string in this crate is held to.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(&dropped_notice(5));
        crate::test_support::assert_no_dashes(&refused_message(1, 2, SinkError::BadRequest));
    }
}
