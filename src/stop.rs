//! A request to stop, as something the run loop can watch without owning
//! signals.
//!
//! The shepherd owns this process's signals and its kill ladder: a dog does
//! not decide when it dies, and every ordinary stop is the shepherd killing
//! the process outright. ctrl-c is the one exception, because it is the
//! clean-exit path for an operator running this binary by hand in a
//! terminal rather than under a shepherd. Watching a request here, rather
//! than reacting to the signal wherever the loop happens to be, is what
//! lets a request already in flight to the shepherd finish rather than
//! being cut off mid write.
//!
//! The run loop owns the signal. It turns ctrl-c into a request here, and
//! watches the same request while it waits on the socket between requests.
//!
//! [`crate::run::run`] calls [`Stop::on_ctrl_c`] and [`Stop::wait`], so a
//! plain (non-test) build reaches everything here except [`Stop::requested`],
//! which only this module's own tests call directly; the run loop learns a
//! stop happened by `wait` resolving, not by polling it.
//!
//! # Every wait on a stop is `biased`
//!
//! A crate-wide convention, and the reason is that `tokio::select!`
//! chooses at random between arms that are both ready. Without `biased`, a
//! loop whose timer fires in the same turn as a stop request has a
//! coin-toss chance of taking another turn: one more flock read, one more
//! refresh of a hundred embeds, one more batch of log lines posted after
//! the operator asked this dog to stop. `biased` with the stop arm first
//! makes that deterministic instead.
//!
//! All five sites follow it: [`wait`] here, [`crate::bot::run`]'s gateway
//! retry loop, [`crate::bot::monitor::refresh`]'s ticker, and both of
//! [`crate::stream::run`]'s own waits, the event loop and the wait for a
//! shepherd's successor.
//!
//! # One request, two watchers
//!
//! Since the gateway came up, this process runs two concurrent loops
//! ([`crate::run::run`]'s streaming loop and [`crate::bot::run`]'s gateway
//! loop),
//! and ctrl-c has to stop both rather than whichever happens to own the
//! original [`Stop`]. [`Stop`] derives `Clone` for exactly that: it wraps a
//! [`watch::Receiver`], which is already cheap to clone and already built
//! to have many readers of one value, so a second loop watching the same
//! request needed no new plumbing, only spelling out that the existing
//! type supports it.

use tokio::sync::watch;

/// Where a stop request is read.
///
/// `Clone` because two concurrent loops now watch one request: see the
/// module doc. Cloning shares the underlying request rather than copying
/// it, the same as cloning any other [`watch::Receiver`].
#[derive(Debug, Clone)]
pub struct Stop(watch::Receiver<bool>);

/// Where a stop request is made. Dropping it without requesting means no
/// request ever comes, which is what a test that never stops wants.
#[derive(Debug)]
pub struct Request(watch::Sender<bool>);

impl Stop {
    /// A stop and the handle that requests it.
    pub fn new() -> (Self, Request) {
        let (sender, receiver) = watch::channel(false);
        (Self(receiver), Request(sender))
    }

    /// A stop nothing will ever request.
    #[cfg(test)]
    pub fn never() -> Self {
        Self::new().0
    }

    /// A stop that ctrl-c requests.
    ///
    /// The listener is a spawned task, so this needs a runtime. If the
    /// handler cannot be installed the dog runs until the shepherd stops it,
    /// which is how it ran before signals were watched at all, and a ctrl-c
    /// then ends it the way the OS does with no handler in place: at once,
    /// without the run loop's chance to finish a request already in
    /// flight. Requesting a stop on that failure instead would exit a dog
    /// nobody asked to stop, and the shepherd would restart it into the
    /// same failure.
    pub fn on_ctrl_c() -> Self {
        let (stop, request) = Self::new();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                request.request();
            }
        });
        stop
    }

    /// Whether a stop has been requested.
    #[allow(
        dead_code,
        reason = "the run loop learns a stop happened from wait resolving, not by polling this; only this module's own tests call it directly"
    )]
    pub fn requested(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolve once a stop is requested, immediately if it already was, and
    /// never if the [`Request`] was dropped without one.
    pub async fn wait(&mut self) {
        // `wait_for` checks the value before it checks for a dropped
        // sender, so a request made before the wait is still seen. It
        // errors only when the sender is gone and the value is still
        // false, which is the "nobody will ever ask" case.
        if self.0.wait_for(|&requested| requested).await.is_err() {
            core::future::pending::<()>().await;
        }
    }
}

impl Request {
    /// Ask every [`Stop`] made with this to stop.
    ///
    /// Takes `&self` rather than consuming, because
    /// [`crate::bot::monitor::refresh::stop`] fires this while the
    /// `Running` that owns it stays in the monitor's own field: taking it
    /// out to fire it is what used to leave a window where a concurrent
    /// start saw no task. Asking twice is harmless; the watch channel
    /// already holds `true`.
    pub fn request(&self) {
        // Nothing to do if every Stop is already gone.
        let _ = self.0.send(true);
    }
}

/// Whether a wait ended in a stop request rather than the clock.
///
/// Shared by [`crate::run::run`]'s streaming loop and [`crate::bot::run`]'s gateway
/// loop: both retry a failed cycle on the same fixed-interval-or-stop
/// shape, so the outcome they both need to branch on is defined once here
/// rather than once per loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupted {
    /// The interval elapsed.
    No,
    /// A stop was requested first, or had been already.
    Yes,
}

/// Sleep for `interval`, or until a stop is requested.
pub async fn wait(interval: core::time::Duration, stop: &mut Stop) -> Interrupted {
    tokio::select! {
        // Biased, stop first: a stop already requested wins over a sleep
        // that is also ready, rather than the coin toss an unbiased select
        // would make of it.
        biased;
        () = stop.wait() => Interrupted::Yes,
        () = tokio::time::sleep(interval) => Interrupted::No,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn a_request_wakes_a_waiter() {
        let (mut stop, request) = Stop::new();
        assert!(!stop.requested());
        let waiter = tokio::spawn(async move {
            stop.wait().await;
            stop.requested()
        });
        request.request();
        assert!(
            timeout(Duration::from_secs(5), waiter)
                .await
                .expect("woke")
                .expect("joined")
        );
    }

    #[tokio::test]
    async fn a_request_made_before_the_wait_still_wakes_it() {
        let (mut stop, request) = Stop::new();
        request.request();
        assert!(stop.requested());
        timeout(Duration::from_secs(5), stop.wait())
            .await
            .expect("a request already made resolves the wait at once");
    }

    #[tokio::test]
    async fn no_request_never_wakes() {
        // Including once the Request is gone: a dropped handle is "nobody
        // will ever ask", not "asked".
        let mut stop = Stop::never();
        assert!(!stop.requested());
        assert!(
            timeout(Duration::from_millis(50), stop.wait())
                .await
                .is_err(),
            "nothing requested a stop, so the wait must not resolve"
        );
        assert!(!stop.requested());
    }
}
