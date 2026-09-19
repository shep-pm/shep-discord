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

use std::sync::Arc;

use shep_client::shep_core::protocol::{ProcessEventKind, ProcessInfo};

use crate::bot::{
    channel,
    monitor::{Monitor, refresh},
};

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
        if !refresh::is_running(&self.monitor) {
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
    use super::*;

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
