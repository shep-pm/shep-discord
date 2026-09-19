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
use std::sync::{Arc, Mutex};

use shep_client::shep_core::values::UpDuration;

use crate::{
    // `Running` is private to the monitor module and visible here because
    // this is one of its children: the task handle and the stop request
    // belong to the engine's own field, and nothing outside gets to build
    // one.
    bot::{
        channel,
        monitor::{Monitor, Running},
    },
    config::Config,
    names::Names,
    shepherd::Live,
    stop::Stop,
};

/// Everything the refresh task needs to draw the flock.
///
/// A struct rather than five parameters: [`start`] would otherwise take a
/// board, a session, a name cache, a flag and a duration positionally, and
/// two of those are easy to swap by mistake.
pub struct Refresh {
    /// Where the monitor draws.
    pub board: channel::Live,
    /// The shepherd session the flock is read from.
    pub live: Arc<Live>,
    /// The name cache every refresh updates, shared with the log stream.
    pub names: Arc<Mutex<Names>>,
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
    pub fn new(
        config: &Config,
        channel: u64,
        interval: UpDuration,
        live: &Arc<Live>,
        names: &Arc<Mutex<Names>>,
    ) -> Self {
        Self {
            board: channel::Live::new(&config.token, channel),
            live: Arc::clone(live),
            names: Arc::clone(names),
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
    if task
        .as_ref()
        .is_some_and(|running| !running.handle.is_finished())
    {
        return false;
    }

    let (mut stop, request) = Stop::new();
    let monitor = Arc::clone(monitor);
    let handle = tokio::spawn(async move {
        let Refresh {
            board,
            live,
            names,
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
                    if let Err(err) = monitor
                        .update_all(&board, &live, &names, ignore_dogs)
                        .await
                    {
                        eprintln!("shep-discord: {err}");
                    }
                }
            }
        }
    });

    *task = Some(Running { handle, request });
    true
}

/// Start the monitor `dogs.toml` asks to run from boot, and say whether
/// one is now running.
///
/// `false`, having done nothing, when the config does not ask: an
/// interval with no channel has nowhere to draw, and a channel with no
/// interval is an operator saying the monitor runs on demand through
/// `/monitor start` rather than from boot. Called once by the run loop
/// rather than on every config reread; see that call site for why.
pub fn start_from_config(
    monitor: &Arc<Monitor>,
    config: &Config,
    live: &Arc<Live>,
    names: &Arc<Mutex<Names>>,
) -> bool {
    let (Some(interval), Some(channel)) = (config.monitor_interval, config.monitor_channel) else {
        return false;
    };
    start(
        monitor,
        Refresh::new(config, channel, interval, live, names),
    )
}

#[cfg(test)]
mod tests {
    use crate::test_support::test_live;

    use super::*;

    /// A config that names no channel, or no interval, asks for no monitor
    /// from boot, and must not leave a task running behind it.
    #[tokio::test]
    async fn a_config_that_does_not_ask_for_a_boot_monitor_starts_nothing() {
        let (live, _fake) = test_live().await;
        let live = Arc::new(live);
        let names = Arc::new(Mutex::new(Names::new()));
        let monitor = Arc::new(Monitor::new());

        for toml in [
            "token = \"t\"\nguild_id = 1\n",
            "token = \"t\"\nguild_id = 1\nmonitor_channel = 7\n",
            "token = \"t\"\nguild_id = 1\nmonitor_interval = \"1m\"\n",
        ] {
            let config = Config::from_toml(toml).expect("parsed");
            assert!(
                !start_from_config(&monitor, &config, &live, &names),
                "{toml:?} does not ask for a monitor from boot"
            );
            assert!(!monitor.is_running());
        }
    }
}
