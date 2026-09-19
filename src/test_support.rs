//! Helpers shared by this crate's unit tests. Compiled under `cfg(test)`
//! only, so nothing here reaches the shipped binary.

use std::sync::{Arc, Mutex};

use shep_client::{
    ReconnectingClient,
    shep_core::{
        protocol::{DogSource, Envelope, ProcessInfo, Request, Response},
        status::ProcStatus,
    },
    testing,
};
use tokio::sync::mpsc;

use crate::shepherd::Live;

/// Assert `text` carries neither an em dash nor an en dash.
///
/// Every string this dog prints for a person is checked with this. A
/// terminal that cannot render either prints a replacement character in
/// the middle of the one message that exists to be read by somebody who is
/// already confused.
#[track_caller]
pub fn assert_no_dashes(text: &str) {
    assert!(!text.contains('\u{2014}'), "em dash in {text:?}");
    assert!(!text.contains('\u{2013}'), "en dash in {text:?}");
}

/// A fake shepherd that answers the one request a test arms, and panics on
/// anything else: every test built on [`test_live`] drives exactly one
/// round trip, so a second or a mismatched request reaching the fake is a
/// test bug, not a daemon behaviour worth scripting around.
///
/// Shared by [`crate::shepherd`]'s own tests and [`crate::bot::commands::shep`]'s:
/// both need one connected [`Live`] talking to one scripted peer, and this
/// was a private copy of the same twelve-line harness in each until the
/// second file needed it too.
pub struct Fake {
    armed: Arc<Mutex<Option<(Request, Response)>>>,
    // Held for their lifetime rather than their value. Dropping the
    // envelope receiver makes the fake's forwarding `send` fail and the
    // connection close before it writes a reply, and dropping the
    // tempdir while `ReconnectingClient` is still holding the file open
    // is a needless way to find out whether that matters on a given
    // platform.
    #[allow(dead_code, reason = "kept only to outlive the fake shepherd")]
    envelopes: mpsc::UnboundedReceiver<Envelope>,
    #[allow(dead_code, reason = "kept only to outlive the fake shepherd")]
    dir: tempfile::TempDir,
}

impl Fake {
    pub fn expect(&mut self, request: Request) -> Armed<'_> {
        Armed {
            fake: self,
            request,
        }
    }
}

/// The half of `fake.expect(request)` waiting on `.answer(response)` or
/// `.answer_error(reason)`.
pub struct Armed<'a> {
    fake: &'a mut Fake,
    request: Request,
}

impl Armed<'_> {
    pub fn answer(self, response: Response) {
        *self.fake.armed.lock().expect("not poisoned") = Some((self.request, response));
    }

    /// Answer with something the caller's own `Live` method cannot use for
    /// the request it asked, so it comes back an `Err` the way a shepherd
    /// that genuinely refused the request would.
    ///
    /// `reason` names the scenario for the test reading it, not the wire:
    /// this harness has no RPC-level error frame to send without pulling
    /// in `tokio-util`'s framing and `shep_core`'s wire internals for one
    /// test's sake, and every `Live` method already turns any response
    /// shaped wrong for its own request into [`crate::error::Error::Unexpected`],
    /// which is indistinguishable, from the caller's side, from a real
    /// refusal.
    pub fn answer_error(self, _reason: &str) {
        self.answer(Response::Pong);
    }
}

/// A connected [`Live`] and the fake shepherd behind it, for a test that
/// arms one request/response pair and drives one [`Live`] call.
pub async fn test_live() -> (Live, Fake) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = testing::control_address(dir.path());
    let armed: Arc<Mutex<Option<(Request, Response)>>> = Arc::new(Mutex::new(None));
    let script = Arc::clone(&armed);
    let envelopes =
        testing::fake_daemon_answering_with_ack(&socket, testing::sample_ack(), move |request| {
            let (expected, response) =
                script
                    .lock()
                    .expect("not poisoned")
                    .take()
                    .unwrap_or_else(|| {
                        panic!("the fake was asked {request:?} before a test armed one")
                    });
            assert_eq!(
                &expected, request,
                "asked something other than what the test armed"
            );
            response
        })
        .await;
    let client = ReconnectingClient::connect(&socket)
        .await
        .expect("test dial");
    (
        Live::new(client),
        Fake {
            armed,
            envelopes,
            dir,
        },
    )
}

/// One sheep, named `name`, for a test that only cares about the name.
pub fn sample(name: &str) -> ProcessInfo {
    ProcessInfo::builder(0, name, ProcStatus::Online).build()
}

/// One dog, named `name`: a [`ProcessInfo`] entry with [`ProcessInfo::dog`]
/// set, the same shape the shepherd reports its own kind of process in a
/// flock listing. For a test proving a sheep listing does not draw the
/// dogs tending it.
pub fn dog_sample(name: &str) -> ProcessInfo {
    ProcessInfo::builder(0, name, ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn))
        .build()
}
