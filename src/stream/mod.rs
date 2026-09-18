//! Turning the log firehose into Discord messages.
//!
//! Three stages: [`buffer`] coalesces raw lines into groups under a bounded
//! queue, [`pack`] turns a group into the chunks and messages Discord's own
//! limits allow, and this module is the loop that joins them to a live bus
//! subscription. [`State`] is the middle of that loop, and it is
//! deliberately plain data plus synchronous methods: [`State::on_event`]
//! takes a [`shep_client::shep_core::protocol::BusEvent`] and returns
//! nothing, so a test drives it with no client, no socket, and no running
//! bot behind it, the same reason `buffer` and `pack` are testable that way.
//! [`run`] is the one place that touches a live [`Live`] session and a real
//! [`Sink`], built by [`State::from_config`] and driven by
//! `tokio::select!`.
//!
//! # Why ctrl-c is the only stop this loop honours
//!
//! There is no `SIGTERM` handler here, and [`run`] never drains [`State`]'s
//! buffers before returning. The shepherd owns this process's signals (see
//! [`crate::stop`]): an ordinary stop is the shepherd killing this process
//! outright, at its default disposition, and whatever is still queued in a
//! [`buffer::Buffer`] is lost with it.
//!
//! That loss costs an operator nothing they cannot already read. Every line
//! this dog ever sends to Discord arrived here as a `log.out` or `log.err`
//! bus event, and the shepherd only emits those as it writes the same bytes
//! to the sheep's own `out_file`/`err_file` on disk. Discord is a mirror of
//! that file, never the system of record, so a mirror's tail going stale
//! across a restart is recoverable with `shep logs` and not worth a single
//! byte of extra risk.
//!
//! The risk a drain-on-stop would add is real, not theoretical. Flushing
//! the buffers on the way out means an HTTP round trip to Discord inside a
//! supervised stop, and that round trip can hang on the network for as long
//! as Discord lets it. shep's own kill ladder is already counting down
//! underneath a stop it is waiting on, so a hung drain trades a loss that
//! costs nothing for a stop that can stall until SIGKILL does the shepherd's
//! job for it. ctrl-c is the one exception, and it stays one: it is the
//! clean-exit path for somebody running this binary by hand in a terminal,
//! not a substitute for the shepherd's own kill ladder.

pub mod buffer;
pub mod discord;
pub mod pack;

use core::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use shep_client::{
    Lagged,
    shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo},
};

use crate::{
    config::Config,
    error::Error,
    names::Names,
    shepherd::Live,
    stop::Stop,
    stream::{
        buffer::{Buffer, Line},
        pack::Chunk,
    },
};

/// Why a [`Sink::send`] failed.
///
/// One variant only, deliberately. A rate limit is not this crate's problem
/// to solve: [`discord::DiscordSink`] sends through `serenity::http::Http`,
/// and that client's own `Ratelimiter` already sleeps on a `retry-after`
/// header and re-sends the request before `send` ever returns, on the
/// default path where `ratelimiter_disabled` is left `false`
/// (`http/client.rs:71`, `http/ratelimiting.rs:236` and `:377` in serenity
/// 0.12.5's vendored source). A second sleep-and-retry here would be a rate
/// limit handled twice, once inside the call this crate makes and once
/// around it, so this type must never grow a variant for one again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    /// Discord refused the request, on its own shape (a 400 and the like)
    /// or for any other reason [`discord::DiscordSink::send`] cannot tell
    /// apart. Retrying a shape rejection sends the identical rejection
    /// forever, which is the failure [`pack`]'s module doc describes the
    /// old code falling into: it sent an oversized batch, took the
    /// refusal, and resent the same batch on every later tick because it
    /// only cleared its queue on success.
    BadRequest,
}

impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest => write!(f, "discord refused the request"),
        }
    }
}

impl core::error::Error for SinkError {}

/// Where a packed batch of chunks actually goes.
///
/// Narrow on purpose, the same reasoning shep-log-rotate's own `Daemon`
/// trait gives for existing at all: with the network behind one method,
/// [`State`] is testable against a recording fake with no client, no
/// socket, and no running bot. [`discord::DiscordSink`] is the one real
/// implementation, and this module's own tests use a recording fake.
///
/// `async fn` in a trait rather than a boxed future: this is used through a
/// generic bound only, never behind `dyn`, so the auto-trait bound an
/// `async fn` in a trait cannot spell is never missed by a caller. This
/// binary has no caller that needs one. The `async_fn_in_trait` lint stays
/// quiet here for the same reason it does in shep-log-rotate's `tick`
/// module: a binary's own trait is not a public surface for anyone to hand
/// a `dyn` object to.
pub trait Sink {
    /// Send every chunk in `chunks` as one Discord message to `channel`.
    ///
    /// # Errors
    /// [`SinkError`] when Discord refuses the message.
    async fn send(&self, channel: u64, chunks: Vec<Chunk>) -> Result<(), SinkError>;
}

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
    /// one means a fresh [`Live::flock`] round trip, and this type does no
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
    /// for the first two, and [`run`] for why the other two need nothing
    /// from this type at all.
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
        sided.buffer.push(Line {
            at_ms: now_ms(),
            name: BUS_NOTICE_NAME.to_owned(),
            text: dropped_notice(count),
        });
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
            sided.buffer.push(Line {
                at_ms: now_ms(),
                name: BUS_NOTICE_NAME.to_owned(),
                text: dropped_notice(dropped),
            });
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

/// Send `message` to `channel`, dropping it and printing why on failure.
///
/// No retry: see [`SinkError`]'s own doc for why a rate limit never reaches
/// here, and [`State::flush`] for why a `BadRequest` is not retried either.
async fn send_or_drop<S: Sink>(sink: &S, channel: u64, message: Vec<Chunk>) {
    let characters = message_characters(&message);
    if let Err(err) = sink.send(channel, message).await {
        eprintln!(
            "shep-discord: channel {channel} refused a {characters} character message, \
             dropping it: {err}"
        );
    }
}

/// Subscribe to this dog's four bus topics and drive [`State`] until `stop`
/// resolves or the subscription itself ends.
///
/// `own_id` is `Some` with the numeric id the shepherd assigned this dog
/// when the caller resolved one through [`Names::id_of`] against the name
/// it announced in its own handshake, and `None` when nobody adopted this
/// process, so there is no id of its own that could ever appear on the bus.
/// See [`State::on_event`] for why a number and not that name is what a
/// `LogOut`/`LogErr` is filtered against, and `crate::session::stream_once`
/// for why a caller never reaches this function at all while an adopted
/// dog's own id has not resolved yet: streaming unfiltered against a bus
/// that does carry this dog's own lines is the bug this project exists to
/// fix. `names` is the cache the caller's own first [`Live::flock`] already
/// populated, so this loop's first flush renders under real names rather
/// than every id's placeholder.
///
/// Subscribes to `log.out` and `log.err` for the lines themselves, and to
/// `process.start` and `process.delete` so the name cache tracks what the
/// flock currently holds; nothing else on the bus changes what a line
/// renders as, and a `Lagged` item ends nothing, matching
/// [`shep_client::EventStream`]'s own contract.
///
/// The `select!` is `biased`, stop first, the same shape shep-log-rotate's
/// own `wait` uses: a stop already requested wins over a flush tick or a
/// bus event that is also ready, rather than the coin toss an unbiased
/// select would make of it. See this module's own doc for why a stop never
/// drains what `state` is still holding.
///
/// # Errors
/// [`Error`] if the initial subscription request fails. Nothing after that
/// point is fatal: a failed flush drops its own batch per [`State::flush`],
/// and a failed muster-roll read on a `process.*` event simply leaves the
/// name cache as it was until the next one succeeds.
pub async fn run<S: Sink>(
    live: &Live,
    config: &Config,
    own_id: Option<u32>,
    names: Names,
    sink: &S,
    stop: &mut Stop,
) -> Result<(), Error> {
    let mut events = live
        .subscribe(vec![
            "log.out".to_owned(),
            "log.err".to_owned(),
            "process.start".to_owned(),
            "process.delete".to_owned(),
        ])
        .await?;

    let mut state = State::from_config(own_id, names, config, sink);
    let mut ticker = tokio::time::interval(config.flush.as_duration());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; nothing is queued yet, so it costs
    // nothing to let it fall on the same schedule as every later one.

    loop {
        tokio::select! {
            biased;
            () = stop.wait() => return Ok(()),
            _ = ticker.tick() => state.flush().await,
            event = events.next() => match event {
                None => return Ok(()),
                Some(Err(lagged)) => state.on_lagged(lagged),
                Some(Ok(BusEvent::Process {
                    event: ProcessEventKind::Start | ProcessEventKind::Delete,
                    ..
                })) => {
                    if let Ok(roll) = live.flock().await {
                        state.refresh_names(&roll);
                    }
                }
                Some(Ok(bus_event)) => state.on_event(bus_event),
            },
        }
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
    #[tokio::test]
    async fn a_dropped_run_is_reported_rather_than_hidden() {
        let sink = Recording::new();
        let mut state = State::new(own_id(9), &sink);
        state.on_event(BusEvent::Dropped { count: 37 });
        state.flush().await;
        let sent = sink.sent();
        assert!(
            sent[0].description.contains("37"),
            "{:?}",
            sent[0].description
        );
        assert!(
            sent[0].description.contains("dropped"),
            "{:?}",
            sent[0].description
        );
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
    #[tokio::test]
    async fn a_lagged_subscription_is_reported_the_same_way() {
        let sink = Recording::new();
        let mut state = State::new(own_id(9), &sink);
        state.on_lagged(Lagged { count: 5 });
        state.flush().await;
        let sent = sink.sent();
        assert!(
            sent[0].description.contains('5'),
            "{:?}",
            sent[0].description
        );
        assert!(
            sent[0].description.contains("dropped"),
            "{:?}",
            sent[0].description
        );
    }
}
