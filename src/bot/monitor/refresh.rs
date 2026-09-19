//! Turning a resolved config into a running refresh task, and ending it.
//!
//! Split from [`super`], the engine, because it changes for its own
//! reasons: what `dogs.toml` offers, how often the flock is redrawn, and
//! what has to happen before the first draw. None of that is read while
//! reasoning about the engine's locking, and none of the locking is read
//! while adding a config key.
//!
//! [`super::Monitor::stop`] stays with the engine rather than living here
//! beside [`start`]: it is the other half of [`super::Monitor::is_running`]
//! and touches the same field, so splitting the pair would put one lock's
//! two readers in two files.

use core::time::Duration;
use std::sync::Arc;

use shep_client::shep_core::values::UpDuration;
use tokio::task::JoinHandle;

use crate::{
    bot::{
        channel,
        monitor::{Board, Monitor},
    },
    config::Config,
    error::Error,
    shepherd::Live,
    stop::{self, Stop},
};

/// A running refresh task and the handle that ends it.
///
/// The handle is an `Option` because [`Monitor::stop`] has to take it out
/// to await it while leaving this `Running` in the monitor's own field:
/// `None` here means a stop is waiting for the task to drain, which
/// [`Running::active`] counts as running for exactly as long as it takes.
pub(super) struct Running {
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) request: stop::Request,
}

impl Running {
    /// Whether this task should stop a new one being spawned beside it.
    ///
    /// True while the task is alive, and true while a [`Monitor::stop`]
    /// holds its handle and waits for it to end. A finished task that
    /// nothing has cleared yet is the only case that answers false, which
    /// is what lets a refresh loop that returned on its own be replaced
    /// without a stop first.
    pub(super) fn active(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(|handle| !handle.is_finished())
    }
}

/// Everything the refresh task needs to draw the flock.
///
/// A struct rather than four parameters: [`start`] would otherwise take a
/// board, a session, a flag and a duration positionally, and two of those
/// are easy to swap by mistake.
pub struct Refresh {
    /// Where the monitor draws.
    pub board: channel::Live,
    /// The shepherd session the flock is read from.
    pub live: Arc<Live>,
    /// Whether other dogs are left out of the monitor, from
    /// [`crate::config::Config::ignore_dogs`].
    pub ignore_dogs: bool,
    /// How often the whole flock is redrawn.
    pub interval: Duration,
}

impl Refresh {
    /// The refresh `config` describes, drawing on `channel` every
    /// `interval`.
    ///
    /// `channel` and `interval` are passed rather than read from
    /// `config` because the two callers disagree about both: the boot
    /// start runs only when `dogs.toml` names an interval, while
    /// `/monitor start` falls back to the floor. What they agree on is
    /// what this reads.
    #[must_use]
    pub fn new(config: &Config, channel: u64, interval: UpDuration, live: &Arc<Live>) -> Self {
        Self {
            board: channel::Live::new(&config.token, channel),
            live: Arc::clone(live),
            ignore_dogs: config.ignore_dogs,
            interval: Duration::from_millis(interval.as_millis()),
        }
    }
}

/// Start the refresh task, and say whether it started.
///
/// `false` when one is already running: `/monitor start` twice must not
/// leave two tasks redrawing the same channel on the same interval.
///
/// A [`tokio::task::JoinHandle`] and a [`crate::stop::Request`], rather
/// than the `setInterval`
/// id the old code kept (`monitor.ts:40`): a handle can be asked whether
/// the task behind it is still alive, which a bare timer id cannot
/// answer, and the stop is the same watch-channel shape every other loop
/// in this dog already waits on. There is no
/// [`tokio_util::sync::CancellationToken`] here because that crate is not
/// a dependency and [`Stop`] is the token this crate already has.
///
/// # What this stop does and does not reach
///
/// The [`crate::stop::Request`] here is private to this task and is held by
/// nothing else, so exactly one thing ever fires it: [`Monitor::stop`],
/// which `/monitor stop` calls and then waits on. That path ends the task
/// properly.
///
/// Ctrl-c does not. `main` builds its own [`Stop`] for the run loop and
/// the gateway, and this task holds neither a clone of it nor anything
/// derived from it; the process exits by dropping the runtime in the
/// background, which drops this task wherever it happened to be. Saying
/// otherwise would be the same failure this project already fixed once by
/// deleting a shutdown path that could never run: a shutdown that only
/// looks reachable is worse than an honest absence of one.
///
/// Nothing is lost by that. At most one post can be in flight when the
/// process goes, so at worst one message lands with its id never cached,
/// and [`crate::bot::channel::rediscover`] adopts it on the next start
/// from the buttons it carries. That is the same recovery a restart
/// already relies on for every other message in the channel.
///
/// [`tokio_util::sync::CancellationToken`]: https://docs.rs/tokio-util
pub fn start(monitor: &Arc<Monitor>, refresh: Refresh) -> bool {
    let mut task = monitor.task.lock().expect("not poisoned");
    if task.as_ref().is_some_and(Running::active) {
        return false;
    }

    let (mut stop, request) = Stop::new();
    let monitor = Arc::clone(monitor);
    let handle = tokio::spawn(async move {
        let Refresh {
            board,
            live,
            ignore_dogs,
            interval,
        } = refresh;

        // Before the first draw, so a restart edits the messages the last
        // run left rather than posting a second one beside each of them.
        // A failed rediscovery is printed and the monitor carries on with
        // an empty cache: drawing a duplicate is worse than an unanswered
        // fetch, but not drawing at all is worse than both.
        //
        // The flock is read first because rediscovery pages, and knowing
        // which sheep it is looking for is what lets it stop paging as
        // soon as it has found them all rather than reading the channel
        // to its beginning. A failed read leaves an empty list, which
        // costs one page; see `channel::rediscover`. The ticker's first
        // tick reads the roll again a moment later, which is one extra
        // request on a monitor's whole lifetime and not worth threading
        // this snapshot through `update_all` to avoid.
        let wanted: Vec<u32> = match live.flock().await {
            Ok(roll) => roll
                .iter()
                .filter(|info| !ignore_dogs || info.dog.is_none())
                .map(|info| info.id)
                .collect(),
            Err(err) => {
                eprintln!("shep-discord: {err}");
                Vec::new()
            }
        };
        match board.me().await {
            Ok(me) => match channel::rediscover(&board, me, &wanted).await {
                Ok(found) => monitor.adopt(found),
                Err(err) => eprintln!("shep-discord: {err}"),
            },
            Err(err) => eprintln!("shep-discord: {err}"),
        }

        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                // Biased, stop first, the same shape every other loop in
                // this dog uses: a stop already requested wins over a tick
                // that is also ready.
                biased;
                () = stop.wait() => return,
                _ = ticker.tick() => {
                    if let Err(err) = monitor.update_all(&board, &live, ignore_dogs).await {
                        eprintln!("shep-discord: {err}");
                    }
                }
            }
        }
    });

    *task = Some(Running {
        handle: Some(handle),
        request,
    });
    true
}

/// Redraw the flock right now, out of the ticker's turn, and say how many
/// sheep were redrawn. `None` when the monitor is not running.
///
/// What `/monitor update` calls, and it lives beside [`start`] rather than
/// with the cache because everything it means is about the schedule: run
/// one pass now, out of turn, without disturbing the ticker. The gate is
/// here rather than in the command for two reasons: it asks the same
/// question [`Monitor::is_running`] answers, and both of its outcomes are
/// then testable against a fake board with no gateway behind them.
///
/// One extra pass, not a new ticker: nothing here touches the task or its
/// schedule, so the next tick falls exactly when it would have. Running
/// alongside that tick is safe for the reasons [`super`] is built on, and
/// this call is not special. The per-sheep guard serialises a manual draw
/// against a scheduled one, so the second to arrive edits what the first
/// posted rather than posting again; each pass reads its own forget counts
/// and its own already-drawn set before its own muster roll, so neither
/// sweeps away what the other just drew nor reposts what the other just
/// deleted.
///
/// # Errors
/// As [`Monitor::update_all`].
pub async fn refresh_now<B: Board>(
    monitor: &Monitor,
    board: &B,
    live: &Live,
    ignore_dogs: bool,
) -> Result<Option<usize>, Error> {
    if !monitor.is_running() {
        return Ok(None);
    }
    monitor.update_all(board, live, ignore_dogs).await.map(Some)
}

/// Start the monitor `dogs.toml` asks to run from boot, and say whether
/// one is now running.
///
/// `false`, having done nothing, when the config does not ask: an
/// interval with no channel has nowhere to draw, and a channel with no
/// interval is an operator saying the monitor runs on demand through
/// `/monitor start` rather than from boot. Called once by the run loop
/// rather than on every config reread; see that call site for why.
pub fn start_from_config(monitor: &Arc<Monitor>, config: &Config, live: &Arc<Live>) -> bool {
    let (Some(interval), Some(channel)) = (config.monitor_interval, config.monitor_channel) else {
        return false;
    };
    start(monitor, Refresh::new(config, channel, interval, live))
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::{
        protocol::{ProcessInfo, Request, Response},
        status::ProcStatus,
    };

    use crate::test_support::{CountingChannel, test_live};

    use super::*;

    /// One sheep, online, named.
    fn info(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    /// `/monitor update` against a monitor nobody started draws nothing
    /// and says so, rather than quietly redrawing a channel the operator
    /// has turned off. The same gate a bus event passes through.
    #[tokio::test]
    async fn a_redraw_is_refused_while_the_monitor_is_off() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();

        // Armed but never expected to be asked: the gate has to turn this
        // away before it reaches the shepherd. Arming it anyway means a
        // broken gate fails on the assertions below, having drawn a sheep
        // it should not have, rather than on the fake's own panic about an
        // unarmed request, which would report the same bug less clearly.
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![info(1, "web")]));

        let redrawn = refresh_now(&monitor, &sink, &live, false)
            .await
            .expect("ok");

        assert_eq!(redrawn, None, "there is no monitor to redraw");
        assert_eq!((sink.sends(), sink.edits(), sink.deletes()), (0, 0, 0));
    }

    /// With the monitor running, a redraw runs one full refresh out of the
    /// ticker's turn and answers with what it drew.
    #[tokio::test]
    async fn a_redraw_while_running_refreshes_the_whole_flock() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();

        // A task that stays alive until dropped, standing in for a real
        // refresh loop, which would need a token and a channel behind it.
        let (mut stop, request) = Stop::new();
        let handle = tokio::spawn(async move { stop.wait().await });
        *monitor.task.lock().expect("not poisoned") = Some(Running {
            handle: Some(handle),
            request,
        });

        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![info(1, "web"), info(2, "api")]));

        let redrawn = refresh_now(&monitor, &sink, &live, false)
            .await
            .expect("ok");

        assert_eq!(redrawn, Some(2), "both sheep were drawn");
        assert_eq!(sink.sends(), 2);
    }

    /// A config that names no channel, or no interval, asks for no monitor
    /// from boot, and must not leave a task running behind it.
    #[tokio::test]
    async fn a_config_that_does_not_ask_for_a_boot_monitor_starts_nothing() {
        let (live, _fake) = test_live().await;
        let live = Arc::new(live);
        let monitor = Arc::new(Monitor::new());

        for toml in [
            "token = \"t\"\nguild_id = 1\n",
            "token = \"t\"\nguild_id = 1\nmonitor_channel = 7\n",
            "token = \"t\"\nguild_id = 1\nmonitor_interval = \"1m\"\n",
        ] {
            let config = Config::from_toml(toml).expect("parsed");
            assert!(
                !start_from_config(&monitor, &config, &live),
                "{toml:?} does not ask for a monitor from boot"
            );
            assert!(!monitor.is_running());
        }
    }
}
