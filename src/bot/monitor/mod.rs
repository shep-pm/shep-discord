//! One message per sheep in the monitor channel, kept up to date in
//! place.
//!
//! This module is the engine: what the monitor remembers about each sheep
//! and what it does to that sheep's message. Two neighbours drive it, and
//! they are separate files because they change for separate reasons.
//! [`refresh`] is the lifecycle, turning a resolved config into a running
//! task on a ticker; [`watch`] is the bus wiring, turning one
//! `process.*` event into a redraw. Neither is read while reasoning about
//! the locking below, and the locking is not read while adding a bus
//! topic. `crate::stream` gave up `crate::stream::state` for the same
//! reason.
//!
//! # The three rules this file is built on
//!
//! **One message per sheep, ever.** A second embed for a sheep already
//! drawn leaves the first behind, and nothing ever edits or deletes it
//! again: it sits in the channel showing a process state that stopped
//! being true the moment it was written. Every other decision here serves
//! this one.
//!
//! **The cache lock is never held across an `await`.** [`Monitor`] holds
//! its id-to-message map behind a `std::sync::Mutex`, locked only for the
//! reads and writes around a Discord call, never during one. A lock held
//! across a round trip serialises every sheep behind the slowest edit, and
//! such a guard is not `Send`, so a spawned task holding one does not
//! compile: the compiler enforces this rule rather than a comment asking
//! for it.
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
//! A failed edit is reported and the cached id kept, never followed by a
//! fresh post. A transient failure heals on the next refresh, which edits
//! the same id again; reposting would put a duplicate in the channel on
//! every blip, and a duplicate is the one failure this file cannot clean
//! up. The cost is a message somebody deleted by hand staying stale until
//! the dog restarts, when [`crate::bot::channel::rediscover`] no longer
//! finds it and the sheep is drawn fresh.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serenity::all::MessageId;
use shep_client::shep_core::protocol::ProcessInfo;
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

use crate::{
    bot::{channel::Board, embed},
    error::Error,
    shepherd::Live,
    stop,
};

pub mod refresh;
pub mod watch;

/// Which message in the monitor channel belongs to which sheep, and the
/// interval task keeping them current.
pub struct Monitor {
    /// What the monitor remembers about each sheep it has drawn or
    /// removed, the whole point of this type.
    messages: Mutex<HashMap<u32, Slot>>,
    /// One lock per sheep, held across that sheep's own Discord call so a
    /// second update for it waits rather than posting a second message.
    /// See [`Monitor::guard_for`].
    guards: Mutex<HashMap<u32, Arc<AsyncMutex<()>>>>,
    /// The refresh task, while one is running. `None` before the first
    /// start and after a stop.
    task: Mutex<Option<Running>>,
}

/// What the monitor remembers about one sheep.
///
/// The forget count is what makes a refresh safe to run against a
/// snapshot that may already be out of date; see [`Monitor::draw`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Slot {
    /// The message drawn for this sheep, while it has one.
    message: Option<MessageId>,
    /// How many times this sheep's message has been forgotten. Only ever
    /// compared against an earlier reading of itself, never interpreted,
    /// so wrapping is not a concern at one increment per delete.
    forgets: u64,
}

/// A running refresh task and the handle that ends it.
struct Running {
    handle: JoinHandle<()>,
    request: stop::Request,
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
    /// keeps the id it just posted, which is newer than anything the
    /// fetch could have seen.
    pub fn adopt(&self, found: HashMap<u32, MessageId>) {
        let mut messages = self.messages.lock().expect("not poisoned");
        for (sheep, message) in found {
            messages
                .entry(sheep)
                .or_default()
                .message
                .get_or_insert(message);
        }
    }

    /// How many times each sheep has been forgotten, as of right now.
    ///
    /// Read before a refresh takes its own muster-roll snapshot, and
    /// carried through to every [`Monitor::draw`] that snapshot feeds, so
    /// a sheep deleted while the refresh is in flight can be told apart
    /// from one that was never drawn. See [`Monitor::draw`] for the race
    /// this closes.
    fn forget_marks(&self) -> HashMap<u32, u64> {
        self.messages
            .lock()
            .expect("not poisoned")
            .iter()
            .map(|(sheep, slot)| (*sheep, slot.forgets))
            .collect()
    }

    /// One sheep's forget count, for a caller acting on a fact about that
    /// sheep alone rather than on a whole-flock snapshot.
    fn forget_mark(&self, id: u32) -> u64 {
        self.messages
            .lock()
            .expect("not poisoned")
            .get(&id)
            .map_or(0, |slot| slot.forgets)
    }

    /// Every sheep that currently has a message drawn for it.
    fn drawn(&self) -> Vec<u32> {
        self.messages
            .lock()
            .expect("not poisoned")
            .iter()
            .filter(|(_, slot)| slot.message.is_some())
            .map(|(sheep, _)| *sheep)
            .collect()
    }

    /// The lock for one sheep, shared by everything that draws or removes
    /// that sheep's message.
    ///
    /// A `tokio::sync::Mutex` rather than a `std::sync::Mutex`, because
    /// this one IS held across a Discord round trip; that is the whole
    /// point of it. The map it lives in is a plain `std::sync::Mutex`,
    /// locked only long enough to clone one `Arc` out of it.
    ///
    /// Entries are never removed, not even by [`Monitor::forget`]: a
    /// caller already waiting holds a clone of the `Arc`, so dropping the
    /// entry would let the NEXT caller mint a fresh, uncontended lock and
    /// post while the first call is still in flight, which is the
    /// duplicate this guard exists to prevent. One pointer per sheep id
    /// ever seen.
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
    /// Reads the sheep's forget count immediately, so this is the entry
    /// point for a caller holding a fact about one sheep that is true
    /// now, such as a bus event. [`Monitor::update_all`] reads its counts
    /// once, before its own muster-roll snapshot, and calls
    /// [`Monitor::draw`] with those instead.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the post or the edit.
    pub async fn update_one<B: Board>(&self, board: &B, info: &ProcessInfo) -> Result<(), Error> {
        self.draw(board, info, self.forget_mark(info.id)).await
    }

    /// Draw `info`, unless the sheep has been forgotten since `mark` was
    /// read.
    ///
    /// The sheep's own guard is taken BEFORE the cache is read, and that
    /// order is half the correctness argument: a second concurrent call
    /// for the same sheep cannot read the cache until the first has
    /// finished writing the id it posted, so it finds that id and edits.
    ///
    /// `mark` is the other half, and it closes a race the guard cannot.
    /// The guard serialises two calls correctly, but neither one knows
    /// its own INPUT was already stale. A refresh reads the flock once
    /// and then draws each sheep in turn; if a `process.delete` for one
    /// of them runs to completion in that gap, this function would find
    /// an empty cache and post a fresh message for a sheep that no longer
    /// exists, which nothing would ever clean up until the next refresh.
    /// The forget count says so without another Discord round trip:
    /// `forget` bumps it, so a count that has moved since `mark` was read
    /// means the caller's snapshot predates a delete, and the post is
    /// skipped. Re-reading the sheep from the shepherd would answer the
    /// same question and would turn one refresh into one request per
    /// sheep.
    ///
    /// Skipping is always safe, never lossy: a sheep that does still
    /// exist is drawn by the next refresh, from a snapshot that includes
    /// the delete this one missed. An edit is not skipped, since a
    /// message that exists is worth correcting whatever happened around
    /// it.
    ///
    /// # Errors
    /// [`Error::Discord`] when Discord refuses the post or the edit.
    async fn draw<B: Board>(&self, board: &B, info: &ProcessInfo, mark: u64) -> Result<(), Error> {
        let guard = self.guard_for(info.id);
        let _in_flight = guard.lock().await;

        let slot = self
            .messages
            .lock()
            .expect("not poisoned")
            .get(&info.id)
            .copied()
            .unwrap_or_default();

        let embed = embed::process_embed(info);
        let buttons = embed::process_buttons(info);
        match slot.message {
            Some(message) => board.edit(message, embed, buttons).await,
            None if slot.forgets != mark => Ok(()),
            None => {
                let message = board.post(embed, buttons).await?;
                self.messages
                    .lock()
                    .expect("not poisoned")
                    .entry(info.id)
                    .or_default()
                    .message = Some(message);
                Ok(())
            }
        }
    }

    /// Delete the monitor message for the sheep `id` names, if it has
    /// one.
    ///
    /// No `Result`: a failed delete leaves a message the next rediscovery
    /// adopts and the sheep is gone either way, so there is nothing a
    /// caller could do with the error that printing it here does not. The
    /// cached id is dropped whether or not the delete succeeds, so a
    /// sheep recreated under the same id is drawn fresh rather than
    /// edited into a message that may no longer exist.
    pub async fn forget<B: Board>(&self, board: &B, id: u32) {
        // The same guard `update_one` takes, so a delete cannot land
        // between that function reading an empty cache and writing the id
        // it just posted, which would leave a message with nothing
        // pointing at it.
        let guard = self.guard_for(id);
        let _in_flight = guard.lock().await;

        let existing = {
            let mut messages = self.messages.lock().expect("not poisoned");
            let slot = messages.entry(id).or_default();
            slot.forgets += 1;
            slot.message.take()
        };
        if let Some(message) = existing
            && let Err(err) = board.delete(message).await
        {
            eprintln!("shep-discord: {err}");
        }
    }

    /// Redraw the whole flock: update every sheep, and delete the message
    /// of every sheep that is no longer there.
    ///
    /// One sheep's failed draw is printed and the rest are still drawn: a
    /// single embed Discord refuses must not leave the other ninety-nine
    /// stale, and the next refresh tries it again.
    ///
    /// Both the forget counts and the set of sheep already drawn are read
    /// BEFORE the muster roll, so everything this function decides is
    /// measured against the same moment. A sheep first drawn after that
    /// moment, by a bus event this refresh could not have seen, is
    /// neither redrawn nor swept away as departed; the next refresh sees
    /// it in its own snapshot.
    ///
    /// Answers with how many sheep it drew successfully, which is what
    /// `/monitor update` reports back to the operator who asked for the
    /// redraw. A sheep whose own draw failed is printed and left out of
    /// that count, so the number is what is current in the channel rather
    /// than what was attempted.
    ///
    /// # Errors
    /// Whatever [`Live::flock`] could not answer. Nothing after that read
    /// is fatal.
    pub async fn update_all<B: Board>(
        &self,
        board: &B,
        live: &Live,
        ignore_dogs: bool,
    ) -> Result<usize, Error> {
        let marks = self.forget_marks();
        let already_drawn = self.drawn();
        let roll = live.flock().await?;

        let drawn: Vec<ProcessInfo> = if ignore_dogs {
            roll.into_iter().filter(|info| info.dog.is_none()).collect()
        } else {
            roll
        };

        let mut redrawn = 0;
        for info in &drawn {
            let mark = marks.get(&info.id).copied().unwrap_or_default();
            match self.draw(board, info, mark).await {
                Ok(()) => redrawn += 1,
                Err(err) => eprintln!("shep-discord: {err}"),
            }
        }

        let present: Vec<u32> = drawn.iter().map(|info| info.id).collect();
        self.sweep(board, &already_drawn, &present).await;
        Ok(redrawn)
    }

    /// Delete the message of every sheep in `already_drawn` that the
    /// flock no longer holds.
    ///
    /// `already_drawn` is the caller's own list, read before it asked for
    /// the muster roll, and not the cache as it stands now. That is the
    /// whole point of the parameter: a sheep first drawn by a bus event
    /// while the refresh was in flight is in the cache but not in either
    /// of the refresh's own two readings, and sweeping against the live
    /// cache would delete the message that event just posted.
    async fn sweep<B: Board>(&self, board: &B, already_drawn: &[u32], present: &[u32]) {
        for id in departed(already_drawn, present) {
            self.forget(board, id).await;
        }
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
    /// The wait is bounded by whatever the task is doing, which is at
    /// worst one refresh of the flock. A Discord request that never
    /// returns is the only thing that could stretch it, and serenity's
    /// HTTP client times out rather than hanging forever.
    pub async fn stop(&self) -> bool {
        // Taken in its own statement so the `std::sync::Mutex` guard is
        // dropped before the await below, which is this module's standing
        // rule and, here, also what stops `stop` from being unable to
        // return a `Send` future to the command that calls it.
        let running = self.task.lock().expect("not poisoned").take();
        let Some(running) = running else {
            return false;
        };
        running.request.request();
        if let Err(err) = running.handle.await {
            eprintln!("{}", task_ended_badly_message(&err));
        }
        true
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

/// What is printed when the refresh task did not end by returning.
///
/// A function rather than an inline `eprintln!`, the same reason `main`'s
/// own `gateway_ended_message` is one: it lets the dash check reach the
/// text without a task to panic first. An ordinary return prints nothing,
/// because that is what stopping is supposed to look like.
fn task_ended_badly_message(err: &tokio::task::JoinError) -> String {
    if err.is_panic() {
        format!(
            "shep-discord: the monitor refresh task panicked: {err}. The messages it drew stay in              the channel, and the next start adopts them."
        )
    } else {
        format!("shep-discord: the monitor refresh task was cancelled: {err}.")
    }
}

/// Which of the sheep already drawn are no longer in the flock, sorted.
///
/// A pure function of two id lists rather than a loop inside
/// [`Monitor::update_all`]: it is the decision that deletes a message, so
/// it is worth being able to exercise on its own. Sorted so a caller's
/// behaviour does not depend on a `HashMap`'s iteration order.
///
/// `cached` is read before the flock snapshot rather than after the
/// drawing loop, so a message posted by a bus event partway through a
/// refresh is not mistaken for one belonging to a departed sheep.
fn departed(cached: &[u32], present: &[u32]) -> Vec<u32> {
    let mut gone: Vec<u32> = cached
        .iter()
        .copied()
        .filter(|id| !present.contains(id))
        .collect();
    gone.sort_unstable();
    gone
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use shep_client::shep_core::{
        protocol::{Request, Response},
        status::ProcStatus,
    };

    use crate::{
        limits::{EMBED_TITLE_LIMIT, MESSAGE_CHARACTER_BUDGET},
        stop::Stop,
        test_support::{CountingChannel, dog_sample, test_live, worst_case_sample},
    };

    use super::*;

    /// One sheep, online, named.
    pub(super) fn info(id: u32, name: &str) -> ProcessInfo {
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

    /// The race the forget count exists for, with the interleaving
    /// forced rather than hoped for: a refresh reads the flock, a
    /// `process.delete` for one of those sheep runs to completion in the
    /// gap, and the refresh then reaches that sheep carrying a snapshot
    /// that predates the delete. Posting there would leave a message for
    /// a sheep that no longer exists, and nothing would clean it up until
    /// the next refresh.
    ///
    /// Driven through [`Monitor::draw`] with the mark a refresh would
    /// have captured, rather than through two concurrent tasks: the
    /// ordering that matters is "the delete finished first", and
    /// scheduling two futures to land that way every time is a flakier
    /// test of a weaker claim.
    #[tokio::test]
    async fn a_delete_that_lands_after_the_snapshot_is_not_undone_by_the_refresh() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        let web = info(1, "web");
        monitor.update_one(&sink, &web).await.expect("ok");

        // What `update_all` reads before it asks for the muster roll.
        let marks = monitor.forget_marks();

        // The event loop's own `process.delete`, start to finish.
        monitor.forget(&sink, 1).await;

        // The refresh loop now reaches sheep 1, still holding its
        // snapshot from before the delete.
        monitor
            .draw(&sink, &web, marks.get(&1).copied().unwrap_or_default())
            .await
            .expect("ok");

        assert_eq!(
            (sink.sends(), sink.deletes()),
            (1, 1),
            "the deleted sheep must not be posted a second time"
        );
    }

    /// The same path with a mark read after the delete rather than
    /// before: a sheep that really was recreated is drawn again, so the
    /// check above skips a stale snapshot rather than skipping every
    /// sheep that has ever been forgotten.
    #[tokio::test]
    async fn a_sheep_drawn_from_a_current_snapshot_is_still_posted() {
        let monitor = Monitor::new();
        let sink = CountingChannel::new();
        let web = info(1, "web");
        monitor.update_one(&sink, &web).await.expect("ok");
        monitor.forget(&sink, 1).await;

        monitor.update_one(&sink, &web).await.expect("ok");

        assert_eq!((sink.sends(), sink.deletes()), (2, 1));
    }

    /// The mirror of the same staleness, on the deleting side: a sheep
    /// first drawn while a refresh is in flight is in the cache by the
    /// time the sweep runs, but in neither of that refresh's own
    /// readings, and its brand new message must survive.
    ///
    /// Driven through `update_all` against a real interleaving, because
    /// the claim is about WHEN `already_drawn` is read and nothing else.
    /// An earlier version of this test called `sweep` with two empty
    /// slices, which `departed` answers with the empty vector for every
    /// implementation, correct or broken: it asserted nothing and
    /// duplicated `only_the_cached_sheep_missing_from_the_flock_are_departed`
    /// below.
    ///
    /// The ordering is forced by the suspension points rather than hoped
    /// for. `tokio::join!` polls the refresh first, which captures its two
    /// readings synchronously and then suspends on the muster-roll round
    /// trip; the draw beside it then runs to completion, because
    /// `CountingChannel` yields once per call while the socket takes
    /// longer than that. So sheep 2 is in the cache, and not in the roll,
    /// by the time the sweep decides anything.
    ///
    /// The regression it exists for: reading `already_drawn` from a live
    /// `self.drawn()` after the drawing loop rather than before
    /// `live.flock()`. That mutation deletes sheep 2's message here, and
    /// no other test in this file notices it.
    #[tokio::test]
    async fn a_sheep_drawn_during_a_refresh_is_not_swept_away_by_it() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();

        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![info(1, "web")]));
        let api = info(2, "api");
        let (refreshed, drawn) = tokio::join!(
            monitor.update_all(&sink, &live, false),
            monitor.update_one(&sink, &api),
        );
        refreshed.expect("ok");
        drawn.expect("ok");

        assert_eq!(
            sink.deletes(),
            0,
            "sheep 2 was drawn after this refresh read which sheep it had messages for, so the \
             refresh knows nothing about it and must not sweep it away"
        );
        assert_eq!(
            sink.sends(),
            2,
            "one message each: the sheep in the roll and the sheep drawn beside it"
        );
    }

    /// A `/monitor stop` followed straight away by a `/monitor start`
    /// must not leave two tasks redrawing one channel, which is what
    /// `start`'s own doc promises. Dropping the handle rather than
    /// joining it broke that promise: the request was sent and the task
    /// kept running while the next start spawned its replacement.
    ///
    /// The flag is set by the task on its way out, so asserting it the
    /// instant `stop` returns is what proves `stop` waited rather than
    /// merely asked. A hand-installed task stands in for the real refresh
    /// loop, which would need a token and a channel behind it.
    #[tokio::test]
    async fn stopping_waits_for_its_task_rather_than_abandoning_it() {
        let monitor = Monitor::new();
        let ended = Arc::new(AtomicBool::new(false));
        let (mut stop, request) = Stop::new();
        let flag = Arc::clone(&ended);
        let handle = tokio::spawn(async move {
            stop.wait().await;
            // A refresh already in flight, finishing after the stop was
            // asked for and before the task returns.
            tokio::task::yield_now().await;
            flag.store(true, Ordering::SeqCst);
        });
        *monitor.task.lock().expect("not poisoned") = Some(Running { handle, request });
        assert!(monitor.is_running());

        assert!(monitor.stop().await);

        assert!(
            ended.load(Ordering::SeqCst),
            "stop returned while its task was still running, so a start could spawn a second one \
             beside it"
        );
        assert!(!monitor.is_running());
        assert!(!monitor.stop().await, "there is nothing left to stop");
    }

    #[test]
    fn only_the_cached_sheep_missing_from_the_flock_are_departed() {
        assert_eq!(departed(&[1, 2, 3], &[2]), vec![1, 3]);
        assert_eq!(departed(&[1], &[1, 2]), Vec::<u32>::new());
        assert_eq!(departed(&[], &[1]), Vec::<u32>::new());
    }

    /// A whole refresh: every sheep is drawn, and the sheep that left
    /// takes its message with it.
    #[tokio::test]
    async fn a_refresh_draws_the_flock_and_deletes_what_left() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();

        monitor.adopt(HashMap::from([(9, MessageId::new(90))]));
        fake.expect(Request::ListFlock)
            .answer(Response::Flock(vec![info(1, "web")]));
        let redrawn = monitor.update_all(&sink, &live, false).await.expect("ok");

        assert_eq!(redrawn, 1, "one sheep drawn, and the count says so");
        assert_eq!(sink.sends(), 1, "the one live sheep is drawn");
        assert_eq!(
            sink.deleted(),
            vec![MessageId::new(90)],
            "the sheep no longer in the flock loses its message"
        );
    }

    /// `ignore_dogs` keeps other dogs out of the monitor. The only test
    /// of that flag being set, so the dog it hides is the whole point.
    #[tokio::test]
    async fn a_dog_is_left_out_of_the_monitor_when_ignore_dogs_is_set() {
        let (live, mut fake) = test_live().await;
        let monitor = Monitor::new();
        let sink = CountingChannel::new();

        // `dog_sample` numbers its dog 0, which is exactly what makes
        // this test readable: the sheep is 1 and the dog is 0.
        fake.expect(Request::ListFlock).answer(Response::Flock(vec![
            info(1, "web"),
            dog_sample("dogsbody"),
        ]));
        monitor.update_all(&sink, &live, true).await.expect("ok");

        assert_eq!(sink.sends(), 1, "the sheep is drawn and the dog is not");
        let posted = sink.posted();
        let json = serde_json::to_value(&posted[0]).expect("json");
        assert_eq!(
            json["title"].as_str().expect("title"),
            "web",
            "the one message in the channel is the sheep's, not the dog's"
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

    /// The one string this module prints for a person that a test can
    /// reach without a task panicking first.
    #[tokio::test]
    async fn nothing_printed_for_a_person_carries_a_dash() {
        let panicked = tokio::spawn(async { panic!("deliberate") })
            .await
            .expect_err("the task panicked");
        crate::test_support::assert_no_dashes(&task_ended_badly_message(&panicked));
    }
}
