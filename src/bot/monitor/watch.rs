//! What one `process.*` bus event asks the monitor to do.
//!
//! Split from [`super`], the engine, because it changes when the bus does:
//! a topic added to [`crate::stream::run`]'s subscription is a line in
//! [`action_for`] and nothing else, and neither the engine's locking nor
//! [`super::refresh`]'s lifecycle is involved in that decision.
//!
//! This is the only part of the monitor [`crate::stream`] names, which is
//! also why it is worth being its own module rather than a corner of the
//! engine's.
//!
//! # Why the board is a type parameter, and why it stops here
//!
//! [`Wired`] is generic over its board for the reason
//! [`super::refresh::Refresh`] is: the board is this crate's seam around
//! Discord, and a concrete one here left [`Wired::on_process_event`] with
//! no test at all. Its first line is the gate deciding whether a bus event
//! draws anything, the same rule [`super::refresh::refresh_now`] enforces
//! for `/monitor update` and has tests on both sides of.
//!
//! The parameter stops at this struct. [`crate::stream::run`] and
//! `crate::session::stream_once` name [`Wired<channel::Live>`](Wired)
//! outright rather than taking a board parameter of their own, because
//! neither one calls a [`Board`] method: they clone an [`Arc`] and hand it
//! on. `run` would then carry two, and the `Sink` it already has is not
//! the same kind of thing. That one is what makes the sink usable at all,
//! since `run` builds a [`crate::stream::state::State`] around it and
//! flushes through it; a board would be a name threaded through a
//! signature and never called.
//!
//! Those two signatures are where to change this, if a test of `run`'s own
//! dispatch ever wants a fake board behind it. One word in each, worth
//! spending when a test asks for it rather than now.

use std::sync::Arc;

use shep_client::shep_core::protocol::{ProcessEventKind, ProcessInfo};

use crate::bot::{
    channel::Board,
    monitor::{Monitor, refresh},
};

/// A monitor and the channel it draws on, for the bus-event side of this
/// dog.
///
/// [`crate::stream::run`] holds one of these and hands it every
/// `process.*` event it sees, so a sheep that stops or comes back online
/// is redrawn the moment it happens rather than on the next interval.
///
/// The board is shared rather than owned, and that `Arc` is the whole
/// point of it: a [`channel::Live`](crate::bot::channel::Live) carries a `serenity::all::Http`, and
/// serenity keeps its rate-limit buckets on that `Http` rather than
/// globally (`http/ratelimiting.rs:84` in the vendored 0.12.5 source,
/// where `routes` and `global` are per `Ratelimiter` and every
/// `Ratelimiter::new` starts them empty). A board of this one's own would
/// write the same channel as [`super::refresh`]'s ticker and
/// `/monitor update` while learning that channel's limits separately from
/// both, so each would have to take its own 429 to find a bucket the
/// others had already found. See [`crate::wiring`] for where the one board
/// is built.
///
/// Generic over that board rather than holding a [`channel::Live`](crate::bot::channel::Live); see
/// this module's own doc for how far the parameter reaches and why it
/// goes no further.
pub struct Wired<B> {
    pub monitor: Arc<Monitor>,
    pub board: Arc<B>,
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

impl<B: Board> Wired<B> {
    /// Redraw, or remove, the sheep one bus event names.
    ///
    /// Does nothing at all while the monitor is off, the gate the old code
    /// kept at `ready.ts:28`: an operator who has not started the monitor
    /// gets no messages in a channel from a bus event either.
    pub async fn on_process_event(&self, kind: ProcessEventKind, info: &ProcessInfo) {
        if !refresh::is_running(&self.monitor) {
            return;
        }
        match action_for(kind) {
            Some(Redraw::Draw) => {
                if let Err(err) = self.monitor.update_one(&*self.board, info).await {
                    eprintln!("shep-discord: {err}");
                }
            }
            Some(Redraw::Forget) => self.monitor.forget(&*self.board, info.id).await,
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serenity::all::MessageId;
    use shep_client::shep_core::status::ProcStatus;

    use crate::test_support::CountingChannel;

    use super::*;

    /// One sheep, online, named.
    fn info(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    /// A [`Wired`] over a board that counts instead of posting, and a
    /// monitor that is off until a test turns it on.
    fn wired() -> (Wired<CountingChannel>, Arc<CountingChannel>) {
        let board = Arc::new(CountingChannel::new());
        let wired = Wired {
            monitor: Arc::new(Monitor::new()),
            board: Arc::clone(&board),
        };
        (wired, board)
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

    /// The gate, and the whole reason a bus event is not simply drawn on
    /// sight: an operator who has not started the monitor gets nothing in
    /// the channel from the bus either. The same rule
    /// [`refresh::refresh_now`] enforces for `/monitor update`.
    ///
    /// A `Draw` kind and a `Forget` kind both, because the gate stands in
    /// front of the whole match rather than inside one of its arms, and a
    /// gate that had slipped into one arm would still turn the other away
    /// if only one were asked.
    #[tokio::test]
    async fn a_monitor_nobody_started_draws_nothing_for_a_bus_event() {
        let (wired, board) = wired();

        wired
            .on_process_event(ProcessEventKind::Online, &info(1, "web"))
            .await;
        wired
            .on_process_event(ProcessEventKind::Delete, &info(1, "web"))
            .await;

        assert_eq!(
            (board.sends(), board.edits(), board.deletes()),
            (0, 0, 0),
            "the monitor is off, so nothing the bus says reaches the channel"
        );
    }

    /// A sheep changing state is drawn the moment the bus says so, which
    /// is what this whole module buys over waiting for the next interval.
    ///
    /// The second event is what proves the draw went through
    /// [`Monitor::update_one`] rather than some other way of posting: only
    /// a path that caches the message id can edit it next time, so an
    /// event that posted twice would leave two embeds for one sheep, the
    /// one failure [`super`] cannot clean up.
    #[tokio::test]
    async fn a_sheep_the_bus_names_is_drawn_and_then_edited_in_place() {
        let (wired, board) = wired();
        refresh::mark_running_with_a_stand_in_task(&wired.monitor);

        wired
            .on_process_event(ProcessEventKind::Online, &info(1, "web"))
            .await;
        assert_eq!((board.sends(), board.edits()), (1, 0), "drawn on sight");

        wired
            .on_process_event(ProcessEventKind::Stop, &info(1, "web"))
            .await;
        assert_eq!(
            (board.sends(), board.edits()),
            (1, 1),
            "the second event edits the message the first posted"
        );
    }

    /// A deleted sheep loses the message drawn for it, rather than leaving
    /// an embed behind showing a process that no longer exists.
    ///
    /// The id is asserted, not only the count: a delete of some other
    /// message would count the same and be a worse bug than no delete at
    /// all.
    #[tokio::test]
    async fn a_deleted_sheep_loses_the_message_the_monitor_drew_for_it() {
        let (wired, board) = wired();
        refresh::mark_running_with_a_stand_in_task(&wired.monitor);
        wired
            .on_process_event(ProcessEventKind::Start, &info(1, "web"))
            .await;

        wired
            .on_process_event(ProcessEventKind::Delete, &info(1, "web"))
            .await;

        assert_eq!(board.deleted(), vec![MessageId::new(1)]);
    }

    /// The kinds [`action_for`] answers `None` for reach the channel in no
    /// other way either, with the monitor running and the gate passed.
    /// [`action_for`]'s own table says what they map to; this says that
    /// mapping is obeyed, which is the half a pure lookup cannot carry.
    #[tokio::test]
    async fn an_event_the_table_ignores_draws_nothing_even_while_running() {
        let (wired, board) = wired();
        refresh::mark_running_with_a_stand_in_task(&wired.monitor);

        for kind in [
            ProcessEventKind::Reload,
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
            ProcessEventKind::Errored,
        ] {
            wired.on_process_event(kind, &info(1, "web")).await;
        }

        assert_eq!(
            (board.sends(), board.edits(), board.deletes()),
            (0, 0, 0),
            "these four are left to the interval refresh"
        );
    }
}
