//! Turning the log firehose into Discord messages.
//!
//! Three stages: [`buffer`] coalesces raw lines into groups under a bounded
//! queue, [`pack`] turns a group into the chunks and messages Discord's own
//! limits allow, and this module is the loop that joins them to a live bus
//! subscription. [`state::State`] is the middle of that loop, and it is
//! deliberately plain data plus synchronous methods: [`state::State::on_event`]
//! takes a [`shep_client::shep_core::protocol::BusEvent`] and returns
//! nothing, so a test drives it with no client, no socket, and no running
//! bot behind it, the same reason `buffer` and `pack` are testable that way.
//! It lives in its own file, [`state`], for the same reason [`discord`]
//! does: it is a whole pipeline stage, worth reading and testing apart from
//! the trait and the loop that drive it. [`run`] is the one place that
//! touches a live [`Live`] session and a real [`Sink`], built by
//! [`state::State::from_config`] and driven by `tokio::select!`.
//!
//! [`run`]'s bus subscription is also where the live monitor hears about a
//! sheep changing state, which is the one place this module reaches into
//! [`crate::bot`]: this loop already has the only `process.*` subscription
//! in the process, and opening a second one for the monitor's sake would
//! double the bus traffic to learn the same facts twice. Nothing else here
//! touches the bot half, and nothing in the bot half opens a stream.
//!
//! # Why ctrl-c is the only stop this loop honours
//!
//! There is no `SIGTERM` handler here, and [`run`] never drains
//! [`state::State`]'s buffers before returning. The shepherd owns this
//! process's signals (see
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
pub mod state;

use core::fmt;
use std::sync::Arc;

use shep_client::{
    LinkLost,
    shep_core::protocol::{BusEvent, ProcessEventKind},
};

use crate::{
    bot::monitor::watch,
    config::Config,
    error::Error,
    names::Names,
    shepherd::Live,
    stop::Stop,
    stream::{pack::Chunk, state::State},
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
/// [`state::State`] is testable against a recording fake with no client, no
/// socket, and no running bot. [`discord::DiscordSink`] is the one real
/// implementation, and [`state`]'s own tests use a recording fake.
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

/// Subscribe to this dog's bus topics and drive [`state::State`], and the
/// live monitor, until `stop` resolves or the subscription itself ends.
///
/// `own_id` is `Some` with the numeric id the shepherd assigned this dog
/// when the caller resolved one against the muster roll's own dog rows and
/// `None` when nobody adopted this process, so there is no id of its own
/// that could ever appear on the bus. See [`state::State::on_event`] for why
/// a number and not that name is what a `LogOut`/`LogErr` is filtered
/// against, and `crate::session::stream_once` for why a caller never
/// reaches this function at all while an adopted dog's own id has not
/// resolved yet: streaming unfiltered against a bus that does carry this
/// dog's own lines is the bug this project exists to fix. `names` is the
/// cache the caller's own first [`Live::flock`] already populated, so this
/// loop's first flush renders under real names rather than every id's
/// placeholder.
///
/// Subscribes to `log.out` and `log.err` for the lines themselves, and to
/// six `process.*` topics. Two of them, `process.start` and
/// `process.delete`, keep the name cache tracking what the flock holds;
/// all six are handed to `monitor`, so a sheep that stops, comes back
/// online, exits or is restarted is redrawn the moment it happens rather
/// than up to a whole refresh interval later. A `Lagged` item ends
/// nothing, matching [`shep_client::EventStream`]'s own contract.
///
/// `monitor` is `Some` only when `dogs.toml` names a `monitor_channel`,
/// and even then [`watch::Wired::on_process_event`] draws nothing while
/// the refresh task is off: that is the gate the old code kept at
/// `ready.ts:28`, and it is what stops a bus event writing to a channel an
/// operator has not turned the monitor on for.
///
/// The redraw is spawned rather than awaited here, and the reason is a
/// burst rather than a single event. A `shep restart` across a fold of
/// thirty sheep puts thirty `process.*` events on the bus at once, each
/// one a Discord edit of a few hundred milliseconds plus whatever the
/// rate limiter adds. Awaiting them in turn takes this loop off the bus
/// for the whole run, and the upstream channel holds 64 items, so the log
/// lines those same restarting sheep are emitting overflow it: the
/// operator gets a `Lagged` notice and a hole in the log channel at the
/// moment they are most likely to be reading it. Spawning keeps
/// `events.next()` and the flush ticker responsive while the edits go
/// out, and [`watch::Wired`] is behind an [`Arc`] so a spawned redraw
/// borrows nothing from this frame.
///
/// Nothing the monitor's locking guarantees is given up by that:
/// [`crate::bot::monitor::Monitor`] takes a per-sheep guard before it
/// reads its cache, so two redraws for one sheep still serialise and the
/// second still finds the message id the first posted. What does change
/// is ORDER between them. Two events for one sheep no longer necessarily
/// reach Discord in the order the bus emitted them, so a `stop` arriving
/// a moment after an `online` can leave the older state drawn. That is
/// bounded and self-correcting: [`crate::bot::monitor::refresh`] redraws
/// every sheep from one fresh muster roll on its interval, which is the
/// same mechanism that already covers the four event kinds
/// [`watch`] deliberately ignores.
///
/// The muster-roll read beside it is still awaited, because the name
/// cache the next event renders against has to be current before that
/// event is handled.
///
/// The `select!` is `biased`, stop first, the same shape shep-log-rotate's
/// own `wait` uses: a stop already requested wins over a flush tick or a
/// bus event that is also ready, rather than the coin toss an unbiased
/// select would make of it. See this module's own doc for why a stop never
/// drains what `state` is still holding.
///
/// # How a shepherd handover is survived
///
/// An [`shep_client::EventStream`] belongs to one connection generation
/// and is not re-armed across a reconnect, so the subscription simply
/// ending is how this dog learns the shepherd handed over. This function
/// used to return there, which left `main`'s own loop to notice on its
/// next pass: a flat [`RESUBSCRIBE_BUDGET`]'s worth of silence in the log
/// channel with nothing printed to say why, and twice that if the
/// reconnect outlasted one cycle. shep does not replay the bus, so every
/// line emitted in that window was gone for good.
///
/// It now waits on [`Live::connected_within`] and subscribes again on the
/// fresh generation. Two things fall out of staying in this function
/// rather than returning: [`state::State`] keeps its buffers, so lines
/// queued when the connection went are flushed after it comes back rather
/// than dropped, and the reconnect is announced on stderr instead of
/// being silent. Giving up after the budget returns `Ok`, handing the
/// retry back to `main`'s loop, which also rereads `dogs.toml` on its way
/// round.
///
/// # Errors
/// [`Error`] if a subscription request fails, the first or a later one.
/// Nothing else is fatal: a failed flush drops its own batch per
/// [`state::State::flush`], and a failed muster-roll read on a `process.*`
/// event is printed and simply leaves the name cache as it was until the
/// next one succeeds.
pub async fn run<S: Sink>(
    live: &Live,
    config: &Config,
    own_id: Option<u32>,
    names: Names,
    sink: &S,
    monitor: Option<&Arc<watch::Wired>>,
    stop: &mut Stop,
) -> Result<(), Error> {
    let mut state = State::from_config(own_id, names, config, sink);
    let mut ticker = tokio::time::interval(config.flush.as_duration());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; nothing is queued yet, so it costs
    // nothing to let it fall on the same schedule as every later one.

    'generation: loop {
        let mut events = live.subscribe(topics()).await?;

        loop {
            tokio::select! {
                biased;
                () = stop.wait() => return Ok(()),
                _ = ticker.tick() => state.flush().await,
                event = events.next() => match event {
                    None => {
                        eprintln!("{}", subscription_ended_message());
                        match wait_for_successor(live, stop).await {
                            Successor::Ready => continue 'generation,
                            Successor::Stopped => return Ok(()),
                            Successor::GaveUp(err) => {
                                eprintln!("{}", no_successor_message(&err));
                                return Ok(());
                            }
                        }
                    }
                    Some(Err(lagged)) => state.on_lagged(lagged),
                    Some(Ok(BusEvent::Process { event, info, .. })) => {
                        // The roll is reread only for the two events that
                        // change which sheep exist; the other four change a
                        // sheep's state, and its name with it stays what the
                        // last read said.
                        if matches!(event, ProcessEventKind::Start | ProcessEventKind::Delete) {
                            match live.flock().await {
                                Ok(roll) => state.refresh_names(&roll),
                                Err(err) => eprintln!("shep-discord: {err}"),
                            }
                        }
                        if let Some(wired) = monitor {
                            let wired = Arc::clone(wired);
                            tokio::spawn(async move {
                                wired.on_process_event(event, &info).await;
                            });
                        }
                    }
                    Some(Ok(bus_event)) => state.on_event(bus_event),
                },
            }
        }
    }
}

/// The bus topics this dog subscribes to, named once because a handover
/// subscribes again with exactly the same list.
fn topics() -> Vec<String> {
    [
        "log.out",
        "log.err",
        "process.start",
        "process.delete",
        "process.stop",
        "process.online",
        "process.exit",
        "process.restart",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// How long [`run`] waits for a successor shepherd before handing the
/// retry back to `main`'s own loop.
///
/// The same length as that loop's own recheck interval, so giving up here
/// costs no more than the cycle it hands back to, and waiting here rather
/// than there is what turns a silent gap into a resubscribe the moment the
/// successor is up.
const RESUBSCRIBE_BUDGET: core::time::Duration = core::time::Duration::from_secs(30);

/// What waiting for a successor shepherd came to.
enum Successor {
    /// The supervisor is on a connection again. Subscribe.
    Ready,
    /// A stop was requested while waiting.
    Stopped,
    /// No connection inside [`RESUBSCRIBE_BUDGET`], or a refusal that no
    /// later wait could fix.
    GaveUp(LinkLost),
}

/// Wait for the client's supervisor to be on a connection again, or for a
/// stop, whichever comes first.
///
/// Biased on the stop, the same shape as every other wait in this dog: an
/// operator pressing ctrl-c during a handover should not sit through the
/// rest of the budget.
async fn wait_for_successor(live: &Live, stop: &mut Stop) -> Successor {
    tokio::select! {
        biased;
        () = stop.wait() => Successor::Stopped,
        outcome = live.connected_within(RESUBSCRIBE_BUDGET) => match outcome {
            Ok(()) => Successor::Ready,
            Err(err) => Successor::GaveUp(err),
        },
    }
}

/// What is printed when the bus subscription ends.
///
/// A function rather than an inline `eprintln!`, the same reason every
/// other person-facing string in this crate is one: it lets the dash check
/// reach the text. Printed at all because the silence was half the defect:
/// an operator watching a log channel go quiet had nothing anywhere saying
/// the shepherd had handed over.
fn subscription_ended_message() -> &'static str {
    "shep-discord: the bus subscription ended, which is how a shepherd handover looks from here. \
     Waiting for the successor before subscribing again; log lines emitted in the meantime are \
     not replayed, and shep logs still has them."
}

/// What is printed when no successor arrived inside the budget.
fn no_successor_message(err: &LinkLost) -> String {
    format!(
        "shep-discord: no successor shepherd to subscribe to: {err}. The run loop will try again \
         on its next pass."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything this file prints for a person. [`state`]'s own tests
    /// carry the dash check for the strings that live there.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(&SinkError::BadRequest.to_string());
        crate::test_support::assert_no_dashes(subscription_ended_message());
        crate::test_support::assert_no_dashes(&no_successor_message(&LinkLost::Budget {
            waited: RESUBSCRIBE_BUDGET,
        }));
    }

    /// Every topic is subscribed again after a handover, because the
    /// resubscribe reads the same list the first subscribe did. A second
    /// hand-written list would be one `process.*` topic away from a
    /// monitor that stops redrawing after the first shepherd restart.
    #[test]
    fn a_resubscribe_asks_for_the_same_topics_as_the_first_one() {
        let topics = topics();
        assert_eq!(
            topics,
            vec![
                "log.out",
                "log.err",
                "process.start",
                "process.delete",
                "process.stop",
                "process.online",
                "process.exit",
                "process.restart",
            ],
            "the log topics the stream reads and the six process topics the monitor redraws on"
        );
    }
}
