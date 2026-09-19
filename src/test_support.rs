//! Helpers shared by this crate's unit tests. Compiled under `cfg(test)`
//! only, so nothing here reaches the shipped binary.

use std::sync::{Arc, Mutex};

use serenity::all::{CreateActionRow, CreateEmbed, MessageId};
use shep_client::{
    ReconnectingClient,
    shep_core::{
        protocol::{DogSource, Envelope, Lamb, ProcessInfo, Request, Response},
        status::ProcStatus,
    },
    testing,
};
use tokio::sync::mpsc;

use crate::{
    bot::channel::{Board, Posted},
    error::Error,
    limits::EMBED_TITLE_LIMIT,
    shepherd::Live,
};

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

/// Assert every string anywhere in `value`, at any depth, carries neither
/// dash: recurses through arrays and objects rather than reading one
/// known field, so a command's own serialized JSON is covered whole.
///
/// A check that only reads `value["description"]` at the top level missed
/// a nested subcommand's own description once already: `/system` has no
/// nesting, so a top-level-only check happened to be enough for the first
/// command this crate registered, and stayed enough right up until
/// `/shep` became the first to declare subcommand and sub-option
/// descriptions of its own, at a level the flat check never reached. A
/// recursive walk is what stops a tenth level of nesting from repeating
/// the same gap a second time.
#[track_caller]
pub fn assert_no_dashes_deep(value: &serde_json::Value) {
    match value {
        serde_json::Value::String(text) => assert_no_dashes(text),
        serde_json::Value::Array(items) => items.iter().for_each(assert_no_dashes_deep),
        serde_json::Value::Object(fields) => fields.values().for_each(assert_no_dashes_deep),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// A [`Board`] that records what the monitor asked it to do and reaches no
/// network at all.
///
/// Shared here rather than written once per test module: [`crate::bot::channel`]
/// needs one to drive [`crate::bot::channel::rediscover`] and
/// [`crate::bot::monitor`] needs the same one to count posts, edits and
/// deletes, and a second copy of the same twenty lines is the drift this
/// crate's own review keeps finding.
///
/// `state` is a `std::sync::Mutex` behind a plain `&self`, never held
/// across the `yield_now` below, so this fake imposes no ordering of its
/// own on the callers it is meant to measure.
#[derive(Default)]
pub struct CountingChannel {
    state: Mutex<ChannelLog>,
}

/// Everything [`CountingChannel`] remembers.
#[derive(Default)]
struct ChannelLog {
    sends: usize,
    edits: usize,
    deletes: usize,
    next_id: u64,
    recent: Vec<Posted>,
    posted: Vec<CreateEmbed>,
    deleted: Vec<MessageId>,
}

impl CountingChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// The messages a later [`Board::recent`] answers with, newest first,
    /// standing in for what a previous run of this dog left in the
    /// channel.
    pub fn preload(&self, messages: Vec<Posted>) {
        self.state.lock().expect("not poisoned").recent = messages;
    }

    pub fn sends(&self) -> usize {
        self.state.lock().expect("not poisoned").sends
    }

    pub fn edits(&self) -> usize {
        self.state.lock().expect("not poisoned").edits
    }

    pub fn deletes(&self) -> usize {
        self.state.lock().expect("not poisoned").deletes
    }

    /// Which message ids were deleted, in the order they were.
    pub fn deleted(&self) -> Vec<MessageId> {
        self.state.lock().expect("not poisoned").deleted.clone()
    }

    /// Every embed posted, in the order they were, for a test that cares
    /// what was drawn rather than only how often.
    pub fn posted(&self) -> Vec<CreateEmbed> {
        self.state.lock().expect("not poisoned").posted.clone()
    }
}

impl Board for CountingChannel {
    /// Yields before recording, deliberately.
    ///
    /// A fake whose whole body runs without an await point lets a
    /// `tokio::join!` of two calls finish the first before the second is
    /// ever polled, so a test for concurrent posting would pass against a
    /// monitor with no guard in it at all. Yielding here is what makes the
    /// second caller reach its own "is there a message yet" question while
    /// the first is still in flight, the way a real HTTP round trip
    /// would.
    async fn post(
        &self,
        embed: CreateEmbed,
        _buttons: CreateActionRow,
    ) -> Result<MessageId, Error> {
        tokio::task::yield_now().await;
        let mut state = self.state.lock().expect("not poisoned");
        state.sends += 1;
        state.next_id += 1;
        state.posted.push(embed);
        Ok(MessageId::new(state.next_id))
    }

    async fn edit(
        &self,
        _id: MessageId,
        _embed: CreateEmbed,
        _buttons: CreateActionRow,
    ) -> Result<(), Error> {
        tokio::task::yield_now().await;
        self.state.lock().expect("not poisoned").edits += 1;
        Ok(())
    }

    async fn delete(&self, id: MessageId) -> Result<(), Error> {
        tokio::task::yield_now().await;
        let mut state = self.state.lock().expect("not poisoned");
        state.deletes += 1;
        state.deleted.push(id);
        Ok(())
    }

    async fn recent(&self) -> Result<Vec<Posted>, Error> {
        tokio::task::yield_now().await;
        Ok(self.state.lock().expect("not poisoned").recent.clone())
    }
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
/// `.answer_wrong_shape(reason)`.
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
    /// Named for what it does rather than for the scenario it stands in
    /// for: it answers `Response::Pong`, a response shaped wrong for
    /// every request but `Ping` itself, not a wire-level RPC error. A
    /// name that only made sense next to this doc comment was worse than
    /// no name at all, since the next caller reads the name before the
    /// comment. `reason` documents the scenario a test is reaching for
    /// (a test bug, not a wire fact); it is not sent anywhere. A real
    /// RPC-level error frame is out of reach here without pulling in
    /// `tokio-util`'s framing and `shep_core`'s wire internals for one
    /// test's sake, and every `Live` method already turns any response
    /// shaped wrong for its own request into [`crate::error::Error::Unexpected`],
    /// which is indistinguishable, from the caller's side, from a real
    /// refusal.
    pub fn answer_wrong_shape(self, _reason: &str) {
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

/// One sheep at the worst-case character width [`crate::bot::embed::embed_character_count`]
/// can produce: title truncated to [`EMBED_TITLE_LIMIT`], and every other
/// field at its own widest form. `id` is the only thing that varies
/// between calls, so a test building several of these to prove a packing
/// decision can also prove none of them was lost or duplicated.
///
/// Shared rather than built by hand in each test that needs it:
/// `bot::embed`'s own `embed_character_count_matches_the_built_embeds_own_json`
/// and `bot::commands::shep`'s packing tests both need the exact same
/// worst case, and a second hand-built copy of thirteen field values is
/// exactly the kind of drift this crate's own review keeps finding.
pub fn worst_case_sample(id: u32) -> ProcessInfo {
    ProcessInfo::builder(id, "a".repeat(EMBED_TITLE_LIMIT + 50), ProcStatus::Online)
        .pid(Some(u32::MAX))
        .restarts(u32::MAX)
        .uptime_ms(u64::MAX)
        .cpu_percent(Some(f32::MIN))
        .memory_bytes(Some(u64::MAX))
        .instance(Some(u32::MAX))
        .lambs(Some(vec![Lamb::new(u32::MAX, "x".repeat(2_000))]))
        .fold(Some("f".repeat(2_000)))
        .smit(Some("s".repeat(2_000)))
        .dog(Some(DogSource::Adopted {
            path: "p".repeat(2_000),
        }))
        .dog_stale(Some(true))
        .build()
}
