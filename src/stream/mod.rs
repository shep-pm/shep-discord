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

use shep_client::shep_core::protocol::{BusEvent, ProcessEventKind};

use crate::{
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

/// Subscribe to this dog's four bus topics and drive [`state::State`] until
/// `stop` resolves or the subscription itself ends.
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
/// point is fatal: a failed flush drops its own batch per
/// [`state::State::flush`], and a failed muster-roll read on a `process.*`
/// event is printed and simply leaves the name cache as it was until the
/// next one succeeds.
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
                })) => match live.flock().await {
                    Ok(roll) => state.refresh_names(&roll),
                    Err(err) => eprintln!("shep-discord: {err}"),
                },
                Some(Ok(bus_event)) => state.on_event(bus_event),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one string this file prints for a person: [`SinkError`]'s
    /// `Display`. [`state`]'s own tests carry the dash check for the
    /// strings that live there.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(&SinkError::BadRequest.to_string());
    }
}
