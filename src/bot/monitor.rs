//! One message per sheep in the monitor channel, kept up to date in
//! place.
//!
//! # The three rules this file is built on
//!
//! **One message per sheep, ever.** A monitor that posts a second embed
//! for a sheep it already drew leaves the first one behind, and nothing
//! ever edits or deletes it again: it sits in the channel showing a
//! process state that stopped being true the moment it was written. Every
//! other decision here serves this one.
//!
//! **The cache lock is never held across an `await`.** [`Monitor`] holds
//! its id-to-message map behind a `std::sync::Mutex`, locked only for the
//! few synchronous reads and writes around a Discord call, never during
//! one. A lock held across a round trip serialises every sheep behind the
//! slowest edit, and a `std::sync::Mutex` guard held across an await does
//! not compile in a spawned task anyway, which is the compiler enforcing
//! the rule rather than a comment asking for it.
//!
//! **The guard is per sheep, not global.** Two updates for the SAME sheep
//! must not both ask "is there a message yet", both hear no, and both
//! post; the second has to wait for the first and then see the id it
//! wrote. That is what [`Monitor::guard_for`] hands out, and it is the
//! best idea in either source repo (`monitor.ts:12`). It is deliberately
//! not one lock over the whole monitor: held across a Discord round trip,
//! a global lock would make a hundred sheep take a hundred serial edits,
//! which is the stall this dog exists to avoid.
//!
//! # What a failed edit does, and does not, do
//!
//! A failed edit is reported and the cached id is kept. It is not
//! followed by a fresh post. A transient failure heals itself on the next
//! refresh, which edits the same id again; reposting instead would put a
//! duplicate embed in the channel on every blip, and a duplicate is the
//! one failure this file has no way to clean up. The cost is a monitor
//! message somebody deleted by hand staying stale until the dog restarts,
//! when [`crate::bot::channel::rediscover`] no longer finds it and the
//! sheep is drawn fresh.

use core::time::Duration;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serenity::all::MessageId;
use shep_client::shep_core::protocol::{ProcessEventKind, ProcessInfo};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

use crate::{
    bot::{
        channel::{self, Board},
        embed,
    },
    error::Error,
    names::Names,
    shepherd::Live,
    stop::{self, Stop},
};

/// Which message in the monitor channel belongs to which sheep, and the
/// interval task keeping them current.
pub struct Monitor {
    /// The sheep id to message id map, the whole point of this type.
    messages: Mutex<HashMap<u32, MessageId>>,
    /// One lock per sheep, held across that sheep's own Discord call so a
    /// second update for it waits rather than posting a second message.
    /// See [`Monitor::guard_for`].
    guards: Mutex<HashMap<u32, Arc<AsyncMutex<()>>>>,
    /// The refresh task, while one is running. `None` before the first
    /// start and after a stop.
    task: Mutex<Option<Running>>,
}

/// A running refresh task and the handle that ends it.
struct Running {
    handle: JoinHandle<()>,
    request: stop::Request,
}

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

impl Monitor {
    /// A monitor with nothing cached and no task running.
    #[must_use]
    pub fn new() -> Self {
        Self {
            messages: Mutex::new(HashMap::new()),
            guards: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
        }
    }

    /// Take on the messages a previous run of this dog left in the
    /// channel, as [`crate::bot::channel::rediscover`] found them.
    ///
    /// Merges rather than replaces: a sheep drawn since the task started
    /// keeps the id it just posted, since that message is newer than
    /// anything the fetch could have seen.
    pub fn adopt(&self, found: HashMap<u32, MessageId>) {
        let mut messages = self.messages.lock().expect("not poisoned");
        for (sheep, message) in found {
            messages.entry(sheep).or_insert(message);
        }
    }

    /// The lock for one sheep, shared by everything that draws or removes
    /// that sheep's message.
    ///
    /// A `tokio::sync::Mutex` rather than a `std::sync::Mutex`, because
    /// this one IS held across a Discord round trip; that is the whole
    /// point of it. The map it lives in is a plain `std::sync::Mutex`,
    /// locked only long enough to clone one `Arc` out of it.
    ///
    /// Entries are never removed, not even by [`Monitor::forget`]. A
    /// caller already waiting holds a clone of the `Arc`, so dropping the
    /// map's entry would let the NEXT caller mint a fresh, uncontended
    /// lock and post while the first call is still in flight, which is the
    /// duplicate this guard exists to prevent. The map is bounded by how
    /// many sheep ids this dog has ever seen, a pointer each.
    fn guard_for(&self, id: u32) -> Arc<AsyncMutex<()>> {
        Arc::clone(
            self.guards
                .lock()
                .expect("not poisoned")
                .entry(id)
                .or_default(),
        )
    }

    /// Draw `info` in the monitor channel: edit the message this sheep
    /// already has, or post its first one.
    ///
    /// The sheep's own guard is taken BEFORE the cache is read, and that
    /// order is the whole correctness argument: a second concurrent call
    /// for the same sheep cannot read the cache until the first has
    /// finished writing the id it posted, so it finds that id and edits.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the post or the edit.
    pub async fn update_one<B: Board>(&self, board: &B, info: &ProcessInfo) -> Result<(), Error> {
        let guard = self.guard_for(info.id);
        let _in_flight = guard.lock().await;

        let existing = self
            .messages
            .lock()
            .expect("not poisoned")
            .get(&info.id)
            .copied();

        let embed = embed::process_embed(info);
        let buttons = embed::process_buttons(info);
        match existing {
            Some(message) => board.edit(message, embed, buttons).await,
            None => {
                let message = board.post(embed, buttons).await?;
                self.messages
                    .lock()
                    .expect("not poisoned")
                    .insert(info.id, message);
                Ok(())
            }
        }
    }

    /// Delete the monitor message for the sheep `id` names, if it has
    /// one.
    ///
    /// No `Result`: a delete that failed leaves a message the next
    /// rediscovery adopts and the sheep is gone either way, so there is
    /// nothing a caller could usefully do with the error that printing it
    /// here does not already do. The cached id is dropped whether or not
    /// the delete succeeds, so a sheep deleted and recreated under the
    /// same id is drawn fresh rather than edited into a message that may
    /// no longer exist.
    pub async fn forget<B: Board>(&self, board: &B, id: u32) {
        // The same guard `update_one` takes, so a delete cannot land
        // between that function reading an empty cache and writing the id
        // it just posted, which would leave a message with nothing
        // pointing at it.
        let guard = self.guard_for(id);
        let _in_flight = guard.lock().await;

        let existing = self.messages.lock().expect("not poisoned").remove(&id);
        if let Some(message) = existing
            && let Err(err) = board.delete(message).await
        {
            eprintln!("shep-discord: {err}");
        }
    }

    /// Redraw the whole flock: refresh the name cache, update every sheep,
    /// and delete the message of every sheep that is no longer there.
    ///
    /// One sheep's failed draw is printed and the rest are still drawn: a
    /// single sheep whose embed Discord refuses must not stop the other
    /// ninety-nine from being current, and the next refresh tries it
    /// again.
    ///
    /// The name cache is refreshed from the whole roll, dogs included,
    /// even when `ignore_dogs` keeps them out of the monitor: [`Names`] is
    /// shared with the log stream, which renders a line by whatever id it
    /// arrives under, and a cache missing the dogs would render those
    /// lines under a placeholder.
    ///
    /// # Errors
    /// Whatever [`Live::flock`] could not answer. Nothing after that read
    /// is fatal.
    pub async fn update_all<B: Board>(
        &self,
        board: &B,
        live: &Live,
        names: &Mutex<Names>,
        ignore_dogs: bool,
    ) -> Result<(), Error> {
        let roll = live.flock().await?;
        names.lock().expect("not poisoned").refresh(&roll);

        let drawn: Vec<ProcessInfo> = if ignore_dogs {
            roll.into_iter().filter(|info| info.dog.is_none()).collect()
        } else {
            roll
        };

        for info in &drawn {
            if let Err(err) = self.update_one(board, info).await {
                eprintln!("shep-discord: {err}");
            }
        }

        let cached: Vec<u32> = self
            .messages
            .lock()
            .expect("not poisoned")
            .keys()
            .copied()
            .collect();
        let present: Vec<u32> = drawn.iter().map(|info| info.id).collect();
        for id in departed(&cached, &present) {
            self.forget(board, id).await;
        }
        Ok(())
    }

    /// Whether a refresh task is running right now.
    ///
    /// The gate the old code kept at `ready.ts:28`: a bus event only draws
    /// while the monitor is on, so a dog with no monitor configured never
    /// writes to a channel an operator did not ask it to write to.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.task
            .lock()
            .expect("not poisoned")
            .as_ref()
            .is_some_and(|running| !running.handle.is_finished())
    }

    /// End the refresh task, and say whether there was one.
    ///
    /// Asks rather than aborts: the task's own `select!` is biased on the
    /// stop, so it returns at the top of its next turn around the loop. A
    /// refresh already in flight finishes its Discord calls first, which
    /// is the difference between a monitor that stops and one that stops
    /// half way through a redraw with an embed posted and its id not yet
    /// cached. The only path this cannot end promptly is a Discord
    /// request that never returns, and serenity's own HTTP client times
    /// out rather than hanging forever.
    pub fn stop(&self) -> bool {
        let Some(running) = self.task.lock().expect("not poisoned").take() else {
            return false;
        };
        running.request.request();
        true
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Which cached sheep are no longer in the flock, sorted.
///
/// A pure function of two id lists rather than a loop inside
/// [`Monitor::update_all`]: it is the decision that deletes a message, so
/// it is worth being able to exercise on its own. Sorted so a caller's
/// behaviour does not depend on a `HashMap`'s iteration order.
fn departed(cached: &[u32], present: &[u32]) -> Vec<u32> {
    let mut gone: Vec<u32> = cached
        .iter()
        .copied()
        .filter(|id| !present.contains(id))
        .collect();
    gone.sort_unstable();
    gone
}

/// Start the refresh task, and say whether it started.
///
/// `false` when one is already running: `/monitor start` twice must not
/// leave two tasks redrawing the same channel on the same interval.
///
/// A [`JoinHandle`] and a [`stop::Request`], rather than the `setInterval`
/// id the old code kept (`monitor.ts:40`): a handle can be asked whether
/// the task behind it is still alive, which a bare timer id cannot
/// answer, and the stop is the same watch-channel shape every other loop
/// in this dog already waits on.
/// There is no [`tokio_util::sync::CancellationToken`] here because that
/// crate is not a dependency and [`Stop`] is the token this crate already
/// has; see the task report.
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
        match board.me().await {
            Ok(me) => match channel::rediscover(&board, me).await {
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

/// A monitor and the channel it draws on, for the bus-event side of this
/// dog.
///
/// [`crate::stream::run`] holds one of these and hands it every
/// `process.*` event it sees, so a sheep that stops or comes back online
/// is redrawn the moment it happens rather than on the next interval.
pub struct Wired {
    pub monitor: Arc<Monitor>,
    pub board: channel::Live,
}

/// What one bus event does to the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redraw {
    /// Draw this sheep's current state.
    Draw,
    /// The sheep is gone; so is its message.
    Forget,
}

/// What `kind` asks the monitor to do, or `None` for an event that changes
/// nothing a monitor message shows.
///
/// A pure function of the event kind so the whole table is testable with
/// no bus, no board and no session. The six kinds here are the ones that
/// change whether a sheep is running or whether it exists at all;
/// `Reload`, `Reloaded`, `ReloadAbandoned` and `Errored` are left to the
/// interval refresh rather than drawn on sight, because each of them
/// arrives in a burst around a restart that already draws, and the
/// interval is what makes the monitor eventually right regardless.
fn action_for(kind: ProcessEventKind) -> Option<Redraw> {
    match kind {
        ProcessEventKind::Start
        | ProcessEventKind::Online
        | ProcessEventKind::Exit
        | ProcessEventKind::Restart
        | ProcessEventKind::Stop => Some(Redraw::Draw),
        ProcessEventKind::Delete => Some(Redraw::Forget),
        _ => None,
    }
}

impl Wired {
    /// Redraw, or remove, the sheep one bus event names.
    ///
    /// Does nothing at all while the monitor is off, the gate the old code
    /// kept at `ready.ts:28`: an operator who has not started the monitor
    /// gets no messages in a channel from a bus event either.
    pub async fn on_process_event(&self, kind: ProcessEventKind, info: &ProcessInfo) {
        if !self.monitor.is_running() {
            return;
        }
        match action_for(kind) {
            Some(Redraw::Draw) => {
                if let Err(err) = self.monitor.update_one(&self.board, info).await {
                    eprintln!("shep-discord: {err}");
                }
            }
            Some(Redraw::Forget) => self.monitor.forget(&self.board, info.id).await,
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use shep_client::shep_core::{
        protocol::{Request, Response},
        status::ProcStatus,
    };

    use crate::{
        limits::{EMBED_TITLE_LIMIT, MESSAGE_CHARACTER_BUDGET},
        test_support::{CountingChannel, dog_sample, test_live, worst_case_sample},
    };

    use super::*;

    fn info(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    /// The per-name guard at `monitor.ts:12` is the best idea in either
    /// source repo: without it two concurrent updates both see "no message
    /// exists" and both post, leaving a duplicate embed nothing will ever
    /// clean up.
    #[tokio::test]
    async fn two_concurrent_updates_for_one_sheep_post_once() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        // Bound rather than inlined into the `join!`: a temporary built
        // inside the macro's own argument list is dropped before the
        // joined futures are awaited.
        let web = info(1, "web");
        let (first, second) = tokio::join!(
            monitor.update_one(&sink, &web),
            monitor.update_one(&sink, &web),
        );
        first.expect("ok");
        second.expect("ok");
        assert_eq!(
            sink.sends(),
            1,
            "the second call must join the first, not race it"
        );
        assert_eq!(sink.edits(), 1, "and then edit what the first posted");
    }

    /// Two sheep must not wait on each other: the guard is per sheep, so
    /// both post, and neither one's draw is serialised behind the other's.
    #[tokio::test]
    async fn two_sheep_each_get_their_own_message() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        let (web, api) = (info(1, "web"), info(2, "api"));
        let (first, second) = tokio::join!(
            monitor.update_one(&sink, &web),
            monitor.update_one(&sink, &api),
        );
        first.expect("ok");
        second.expect("ok");
        assert_eq!(sink.sends(), 2);
        assert_eq!(sink.edits(), 0);
    }

    #[tokio::test]
    async fn a_deleted_sheep_loses_its_monitor_message() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        monitor
            .update_one(&sink, &info(1, "web"))
            .await
            .expect("ok");
        monitor.forget(&sink, 1).await;
        assert_eq!(sink.deletes(), 1);
    }

    /// A sheep the monitor never drew has no message to delete, and asking
    /// Discord to delete one anyway is a request that can only fail.
    #[tokio::test]
    async fn forgetting_an_undrawn_sheep_deletes_nothing() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        monitor.forget(&sink, 1).await;
        assert_eq!(sink.deletes(), 0);
    }

    /// The second draw of the same sheep edits the message the first one
    /// posted. This is the whole feature: one message per sheep, kept
    /// current, rather than a channel full of history.
    #[tokio::test]
    async fn a_later_update_edits_rather_than_posting_again() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        monitor
            .update_one(&sink, &info(1, "web"))
            .await
            .expect("ok");
        monitor
            .update_one(&sink, &info(1, "web"))
            .await
            .expect("ok");
        assert_eq!((sink.sends(), sink.edits()), (1, 1));
    }

    /// A restart adopts what the last run left behind, so the first draw
    /// after a restart is an edit and the channel does not grow a second
    /// embed for every sheep.
    #[tokio::test]
    async fn an_adopted_message_is_edited_rather_than_reposted() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        monitor.adopt(HashMap::from([(1, MessageId::new(42))]));
        monitor
            .update_one(&sink, &info(1, "web"))
            .await
            .expect("ok");
        assert_eq!((sink.sends(), sink.edits()), (0, 1));
    }

    #[test]
    fn only_the_cached_sheep_missing_from_the_flock_are_departed() {
        assert_eq!(departed(&[1, 2, 3], &[2]), vec![1, 3]);
        assert_eq!(departed(&[1], &[1, 2]), Vec::<u32>::new());
        assert_eq!(departed(&[], &[1]), Vec::<u32>::new());
    }

    /// A whole refresh: the name cache follows the roll, every sheep is
    /// drawn, and the sheep that left takes its message with it.
    #[tokio::test]
    async fn a_refresh_draws_the_flock_and_deletes_what_left() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        let names = Mutex::new(Names::new());

        monitor.adopt(HashMap::from([(9, MessageId::new(90))]));
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![info(1, "web")]));
        monitor
            .update_all(&sink, &live, &names, false)
            .await
            .expect("ok");

        assert_eq!(sink.sends(), 1, "the one live sheep is drawn");
        assert_eq!(
            sink.deleted(),
            vec![MessageId::new(90)],
            "the sheep no longer in the flock loses its message"
        );
        assert_eq!(names.lock().expect("not poisoned").get(1), "web");
    }

    /// `ignore_dogs` keeps other dogs out of the monitor, but not out of
    /// the name cache the log stream renders lines with.
    #[tokio::test]
    async fn ignoring_dogs_still_names_them_for_the_log_stream() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        let names = Mutex::new(Names::new());

        // `dog_sample` numbers its dog 0, which is exactly what makes
        // this test readable: the sheep is 1 and the dog is 0.
        fake.expect(Request::ListFlock).answer(Response::Flock(vec![
            info(1, "web"),
            dog_sample("dogsbody"),
        ]));
        monitor
            .update_all(&sink, &live, &names, true)
            .await
            .expect("ok");

        assert_eq!(sink.sends(), 1, "the dog is not drawn");
        assert_eq!(
            names.lock().expect("not poisoned").get(0),
            "dogsbody",
            "but a log line from it still renders under its name"
        );
    }

    /// A name longer than Discord's embed title limit must reach the
    /// channel fitted, not refused: a 400 here loses the whole message,
    /// and with it this sheep's only monitor entry.
    #[tokio::test]
    async fn a_sheep_named_past_the_title_limit_is_still_drawn() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        let sheep = worst_case_sample(1);
        assert!(
            sheep.name.chars().count() > EMBED_TITLE_LIMIT,
            "the input has to exceed the limit for this to prove anything"
        );

        monitor.update_one(&sink, &sheep).await.expect("ok");

        let posted = sink.posted();
        let json = serde_json::to_value(&posted[0]).expect("json");
        let title = json["title"].as_str().expect("title");
        assert_eq!(title.chars().count(), EMBED_TITLE_LIMIT);
        assert!(
            embed::embed_character_count(&sheep) <= MESSAGE_CHARACTER_BUDGET,
            "one sheep's own embed has a whole message's budget to itself here"
        );
    }

    /// The table of what the bus asks the monitor to do. `Reload` and its
    /// neighbours are deliberately absent: see [`action_for`].
    #[test]
    fn every_process_event_the_monitor_acts_on_is_named_here() {
        for kind in [
            ProcessEventKind::Start,
            ProcessEventKind::Online,
            ProcessEventKind::Exit,
            ProcessEventKind::Restart,
            ProcessEventKind::Stop,
        ] {
            assert_eq!(action_for(kind), Some(Redraw::Draw), "{kind:?}");
        }
        assert_eq!(action_for(ProcessEventKind::Delete), Some(Redraw::Forget));
        for kind in [
            ProcessEventKind::Reload,
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
            ProcessEventKind::Errored,
        ] {
            assert_eq!(action_for(kind), None, "{kind:?}");
        }
    }
}
