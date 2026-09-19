//! Turning a resolved config into a running refresh task, and ending it.
//!
//! Split from [`super`], the engine, because it changes for its own
//! reasons: what `dogs.toml` offers, how often the flock is redrawn, and
//! what has to happen before the first draw. None of that is read while
//! reasoning about the engine's locking, and none of the locking is read
//! while adding a config key.
//!
//! Every operation on the monitor's task lives here: [`start`] spawns
//! that task, [`is_running`] reports whether one is up and [`stop()`] ends
//! it. The field itself is declared on [`Monitor`] next door, because it
//! is a field of that struct, and the engine does nothing with it beyond
//! setting it to `None` in `Monitor::new`. Holding the three together is
//! the point rather than a side effect: they are the only readers of one
//! lock, and the handshake between them is subtle enough that reading
//! half of it in another file is how it was got wrong once already.
//!
//! [`stop()`] is written with parentheses wherever it is linked because
//! this module imports [`crate::stop`] as well, and a bare `stop` would
//! be a link to either. The function keeps the name regardless: what the
//! two command call sites read is [`start`] beside [`stop()`], and that
//! pairing is worth more than the one character.

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
/// The handle is an `Option` because [`stop()`] has to take it out
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
    /// True while the task is alive, and true while a [`stop()`]
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
///
/// Generic over the board rather than holding a [`channel::Live`], which
/// is what it used to hold. [`Board`] is where this crate puts the network
/// so the monitor can be reasoned about without one, and a concrete board
/// here stopped that seam one layer short of the thing that starts the
/// monitor at all: every decision about a message was testable and the
/// decision to draw in the first place was not. Nothing is spelled out at
/// a call site that was not already, since the only board this ships with
/// is still the only one [`start_from_config`] is ever handed.
pub struct Refresh<B> {
    /// Where the monitor draws.
    pub board: B,
    /// The shepherd session the flock is read from.
    pub live: Arc<Live>,
    /// Whether other dogs are left out of the monitor, from
    /// [`crate::config::Config::ignore_dogs`].
    pub ignore_dogs: bool,
    /// How often the whole flock is redrawn.
    pub interval: Duration,
}

impl<B: Board> Refresh<B> {
    /// The refresh `config` describes, drawing on `board` every
    /// `interval`.
    ///
    /// `board` and `interval` are passed rather than built from `config`
    /// because the two callers disagree about both: the boot start runs
    /// only when `dogs.toml` names an interval, while `/monitor start`
    /// falls back to the floor. What they agree on is what this reads.
    #[must_use]
    pub fn new(board: B, config: &Config, interval: UpDuration, live: &Arc<Live>) -> Self {
        Self {
            board,
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
/// nothing else, so exactly one thing ever fires it: [`stop()`], which
/// `/monitor stop` calls and then waits on. That path ends the task
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
/// # Why the board is bound the way it is
///
/// `Send + Sync + 'static` is what `tokio::spawn` asks of anything the
/// task owns and then borrows across an `await`, and the task does both:
/// it takes the board by value and hands out `&board` to a rediscovery
/// and to every refresh after it. [`channel::Live`] satisfies all three,
/// being a serenity `Http` and a channel id and owning both.
///
/// [`tokio_util::sync::CancellationToken`]: https://docs.rs/tokio-util
pub fn start<B: Board + Send + Sync + 'static>(
    monitor: &Arc<Monitor>,
    refresh: Refresh<B>,
) -> bool {
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

/// Whether a refresh task is running right now.
///
/// The gate the old code kept at `ready.ts:28`: a bus event only draws
/// while the monitor is on, so a dog with no monitor configured never
/// writes to a channel an operator did not ask it to write to.
///
/// A free function taking `&Monitor`, as [`start`] and [`refresh_now`]
/// beside it already are. What it reads is the task, which is this
/// module's subject, rather than the cache, which is the engine's.
#[must_use]
pub fn is_running(monitor: &Monitor) -> bool {
    monitor
        .task
        .lock()
        .expect("not poisoned")
        .as_ref()
        .is_some_and(Running::active)
}

/// End the refresh task, wait for it to finish, and say whether
/// there was one.
///
/// Asks rather than aborts: the task's `select!` is biased on the
/// stop, so it returns at the top of its next turn around the loop,
/// and a refresh already in flight finishes its Discord calls first
/// rather than stopping half way through a redraw with an embed
/// posted and its id not yet cached.
///
/// Then waits, which is the part that matters for correctness rather
/// than tidiness. Dropping the handle and returning would let a
/// `/monitor stop` immediately followed by a `/monitor start` leave
/// two refresh tasks running against one monitor, and two tasks are
/// worse than none: one task's departed sweep can delete a message
/// the other posted moments earlier for a sheep that is perfectly
/// alive. Only the caller can wait, because only the caller knows it
/// is allowed to: `/monitor stop` is a command with a deferred
/// interaction behind it, so it has minutes to answer in, and every
/// other path to here is a test.
///
/// Waiting is not enough on its own, and this used to take the whole
/// [`Running`] out of the field before awaiting it. That left the
/// field `None` for the length of the drain, so a `/monitor start`
/// arriving in that window found nothing running and spawned its own
/// task beside the one still finishing: the exact pair of tasks the
/// wait exists to prevent, reachable by two operators or by one
/// impatient one, since serenity dispatches every interaction on its
/// own task. Only the handle is taken out now. [`Running`] stays
/// where it is until the task has ended, so [`is_running`] keeps
/// answering true and that start is refused.
///
/// A second concurrent `stop` finds the handle already taken. It
/// answers true without waiting and without clearing the field,
/// leaving that to the call that holds the handle: two callers both
/// get the truth, that the task is on its way out, and only one of
/// them can say when it is gone.
///
/// The wait is bounded by whatever the task is doing, which is at
/// worst one refresh of the flock. A Discord request that never
/// returns is the only thing that could stretch it, and serenity's
/// HTTP client times out rather than hanging forever.
pub async fn stop(monitor: &Monitor) -> bool {
    // Scoped so the `std::sync::Mutex` guard is dropped before the
    // await below, which is the engine's standing rule and, here,
    // also what stops this from being unable to return a `Send`
    // future to the command that calls it.
    let handle = {
        let mut task = monitor.task.lock().expect("not poisoned");
        let Some(running) = task.as_mut() else {
            return false;
        };
        running.request.request();
        running.handle.take()
    };
    let Some(handle) = handle else {
        // Another `stop` is already waiting on this task and will
        // clear the field when it ends.
        return true;
    };
    if let Err(err) = handle.await {
        eprintln!("{}", task_ended_badly_message(&err));
    }
    *monitor.task.lock().expect("not poisoned") = None;
    true
}

/// Redraw the flock right now, out of the ticker's turn, and say how many
/// sheep were redrawn. `None` when the monitor is not running.
///
/// What `/monitor update` calls, and it lives beside [`start`] rather than
/// with the cache because everything it means is about the schedule: run
/// one pass now, out of turn, without disturbing the ticker. The gate is
/// here rather than in the command for two reasons: it asks the same
/// question [`is_running`] answers, and both of its outcomes are then
/// testable against a fake board with no gateway behind them.
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
    if !is_running(monitor) {
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
///
/// `new_board` is how the board is built, and the run loop passes
/// [`channel::Live::new`]. It is a parameter rather than that call
/// written inline because the branch this function exists for is the one
/// that spawns a task, and that task asks Discord who this bot is before
/// its first draw: with the board built in here, proving that a
/// `dogs.toml` asking for a monitor gets one meant putting a real request
/// behind a unit test, so nothing proved it. It is called only when the
/// config does ask, so a config wanting no monitor still builds nothing.
pub fn start_from_config<B: Board + Send + Sync + 'static>(
    monitor: &Arc<Monitor>,
    config: &Config,
    live: &Arc<Live>,
    new_board: impl FnOnce(&str, u64) -> B,
) -> bool {
    let (Some(interval), Some(channel)) = (config.monitor_interval, config.monitor_channel) else {
        return false;
    };
    let board = new_board(&config.token, channel);
    start(monitor, Refresh::new(board, config, interval, live))
}

/// What is printed when the refresh task did not end by returning.
///
/// A function rather than an inline `eprintln!`, the same reason
/// [`crate::run`]'s own `gateway_ended_message` is one: it lets the dash check reach the
/// text without a task to panic first. An ordinary return prints nothing,
/// because that is what stopping is supposed to look like.
pub(super) fn task_ended_badly_message(err: &tokio::task::JoinError) -> String {
    if err.is_panic() {
        format!(
            "shep-discord: the monitor refresh task panicked: {err}. The messages it drew stay in              the channel, and the next start adopts them."
        )
    } else {
        format!("shep-discord: the monitor refresh task was cancelled: {err}.")
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

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
                !start_from_config(
                    &monitor,
                    &config,
                    &live,
                    |_token, _channel| -> CountingChannel {
                        unreachable!("{toml:?} asks for no board to be built")
                    }
                ),
                "{toml:?} does not ask for a monitor from boot"
            );
            assert!(!is_running(&monitor));
        }
    }

    /// The other half of that decision, and the one that actually spawns
    /// something. A config naming both a channel and an interval starts
    /// the monitor, and builds its board for the channel and the token
    /// that config names rather than for anything else.
    ///
    /// The fake board is what makes this reachable at all. The refresh
    /// task asks Discord who this bot is before its first draw, so a
    /// `channel::Live` here would put a real request behind a unit test.
    #[tokio::test]
    async fn a_config_that_asks_for_a_boot_monitor_starts_one() {
        let (live, mut fake) = test_live().await;
        let live = Arc::new(live);
        let monitor = Arc::new(Monitor::new());
        let config = Config::from_toml(
            "token = \"t\"\nguild_id = 1\nmonitor_channel = 7\nmonitor_interval = \"1m\"\n",
        )
        .expect("parsed");

        // The task reads the flock once before its first draw, and an
        // empty one is enough: what is under test is that a task exists
        // to read it at all.
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(Vec::new()));

        let mut built_for = None;
        let started = start_from_config(&monitor, &config, &live, |token, channel| {
            built_for = Some((token.to_owned(), channel));
            CountingChannel::new()
        });

        assert!(started, "this config does ask for a monitor from boot");
        assert!(is_running(&monitor));
        assert_eq!(
            built_for,
            Some(("t".to_owned(), 7)),
            "the board is built for the token and channel dogs.toml names"
        );
        assert!(stop(&monitor).await, "the task it started is there to stop");
    }

    /// The one string this module writes itself. The others it prints
    /// are an [`Error`]'s own `Display`, swept in `crate::error`.
    #[tokio::test]
    async fn nothing_printed_for_a_person_carries_a_dash() {
        let panicked = tokio::spawn(async { panic!("deliberate") })
            .await
            .expect_err("the task panicked");
        crate::test_support::assert_no_dashes(&task_ended_badly_message(&panicked));
    }

    /// A `/monitor stop` followed straight away by a `/monitor start`
    /// must not leave two tasks redrawing one channel, which is what
    /// [`start`]'s own doc promises. Dropping the handle rather than
    /// joining it broke that promise: the request was sent and the task
    /// kept running while the next start spawned its replacement.
    ///
    /// The flag is set by the task on its way out, so asserting it the
    /// instant [`stop()`] returns is what proves it waited rather than
    /// merely asked. A hand-installed task stands in for the real refresh
    /// loop, which would need a token and a channel behind it.
    ///
    /// The receiving half of the [`Stop`] is `signal` here rather than
    /// `stop`, which is what [`start`] calls it: a local of that name
    /// would shadow the [`stop()`] this test is about.
    #[tokio::test]
    async fn stopping_waits_for_its_task_rather_than_abandoning_it() {
        let monitor = Monitor::new();
        let ended = Arc::new(AtomicBool::new(false));
        let (mut signal, request) = Stop::new();
        let flag = Arc::clone(&ended);
        let handle = tokio::spawn(async move {
            signal.wait().await;
            // A refresh already in flight, finishing after the stop was
            // asked for and before the task returns.
            tokio::task::yield_now().await;
            flag.store(true, Ordering::SeqCst);
        });
        *monitor.task.lock().expect("not poisoned") = Some(Running {
            handle: Some(handle),
            request,
        });
        assert!(is_running(&monitor));

        assert!(stop(&monitor).await);

        assert!(
            ended.load(Ordering::SeqCst),
            "stop returned while its task was still running, so a start could spawn a second one \
             beside it"
        );
        assert!(!is_running(&monitor));
        assert!(!stop(&monitor).await, "there is nothing left to stop");
    }

    /// The window between the stop being asked for and the task actually
    /// ending belongs to that task, so the monitor has to keep claiming
    /// to be running throughout it. [`start`] refuses while
    /// [`is_running`] is true, and that refusal is the only thing standing
    /// between an impatient operator and two refresh loops on one
    /// channel: serenity dispatches every interaction on its own task, so
    /// a `/monitor start` can land while a `/monitor stop` is still
    /// waiting.
    ///
    /// Nothing here sleeps. The task announces that it has seen the stop
    /// and then blocks until this test releases it, so the assertion
    /// falls inside the drain window by construction rather than by
    /// timing.
    #[tokio::test]
    async fn a_monitor_counts_as_running_while_its_task_is_draining() {
        let monitor = Monitor::new();
        let draining = Arc::new(AtomicBool::new(false));
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (mut signal, request) = Stop::new();
        let flag = Arc::clone(&draining);
        let handle = tokio::spawn(async move {
            signal.wait().await;
            // A refresh still finishing its Discord calls after the stop
            // was asked for: exactly what `stop` waits out.
            flag.store(true, Ordering::SeqCst);
            let _ = released.await;
        });
        *monitor.task.lock().expect("not poisoned") = Some(Running {
            handle: Some(handle),
            request,
        });

        let watch = async {
            while !draining.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            let running = is_running(&monitor);
            let _ = release.send(());
            running
        };
        let (stopped, running_mid_drain) = tokio::join!(stop(&monitor), watch);

        assert!(stopped);
        assert!(
            running_mid_drain,
            "the monitor reported itself idle while its task was still draining, so a start \
             racing this stop would have spawned a second refresh loop"
        );
        assert!(!is_running(&monitor), "and idle once the task is gone");
    }
}
