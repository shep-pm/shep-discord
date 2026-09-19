//! The one module that talks to the shepherd.
//!
//! [`Live`] wraps a [`ReconnectingClient`] and is the only place in this
//! crate that builds a [`Request`] or reads a [`Response`]. Everything else
//! that needs the shepherd goes through the methods here instead of
//! matching on the wire types itself, so a protocol change touches one
//! file rather than every call site that happened to need a sheep's
//! status.
//!
//! # PM2's verbs become shep's
//!
//! [`Verb`] is the vocabulary a Discord command names: start, stop,
//! restart, reload, delete, flush, save, reopen. [`Live::act`] is where
//! each one becomes the [`Request`] shep actually answers, and the mapping
//! is not one-to-one. [`Verb::Start`] asks [`Request::Restart`], not
//! `Request::Start`: `Start` registers apps from an `AppConfig` this bot
//! never holds, and on a name the flock already has it adds instances
//! rather than starting the stopped one. A Discord operator who types
//! "start" wants the sheep that is already registered running again, which
//! is what a restart does; the reply still says "started", because that is
//! the word the operator used and the outcome they asked for.
//!
//! [`Verb::Save`] is the one verb with no target: it asks
//! [`Request::SaveRoll`], which snapshots the whole muster roll rather than
//! one sheep, so the `selector` [`Live::act`] otherwise threads through
//! every other verb is built but unused on that path.
//!
//! # Naming a reply
//!
//! Every other verb answers with the sheep it reached, and the reply
//! sentence names them from that answer rather than from what was asked:
//! a selector like [`SelectorSpec::Fold`] can match several sheep, and the
//! response is the only place that says which ones the shepherd actually
//! touched. [`Request::Delete`]'s answer is the one exception, a bare list
//! of ids with no name attached, so its sentence falls back to describing
//! the selector instead.
//!
//! [`Request::Restart`] and [`Request::Reload`] can also answer with a walk
//! that reached some sheep and refused others outright. Naming only the
//! accepted set would hide that refusal from the one place an operator
//! would see it, so the sentence names both when `refused` is not empty.

use core::fmt;

use shep_client::{
    EventStream, LinkLost, LinkState, ReconnectingClient,
    shep_core::protocol::{HostUsage, ProcessInfo, Request, Response, SelectorSpec, SheepRefusal},
};

use crate::error::Error;

/// A PM2-shaped verb, as `/shep` names one.
///
/// One entry per operational request the shepherd answers,
/// [`Request::Flush`] included: this enum and [`Live::act`] are the only
/// place that variant is built, so a `Request::Flush` anywhere else in this
/// crate is a review finding on sight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Restart what is already registered. See the module doc for why this
    /// does not register anything.
    Start,
    /// Stop matching sheep, staying registered.
    Stop,
    /// Restart matching sheep.
    Restart,
    /// Replace matching sheep with a fresh instance, one at a time.
    Reload,
    /// Stop and deregister matching sheep.
    Delete,
    /// Empty matching sheep's log files.
    Flush,
    /// Write the muster roll now. The only verb with no target: see the
    /// module doc.
    Save,
    /// Reopen matching sheep's log files, for an external rotator.
    Reopen,
}

/// A marker whose `Debug` always prints the same placeholder, never the
/// value it stands in for.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted>")
    }
}

/// A connected session with the shepherd, and the only type in this crate
/// that builds a [`Request`] or reads a [`Response`].
///
/// `Debug` is hand-written rather than derived. [`ReconnectingClient`]
/// holds the socket path it dialled, which is `$SHEP_HOME/run/shep.sock`
/// and so usually sits under somebody's home directory; a derived `Debug`
/// here would put that path in any log line or panic message that prints a
/// `Live` in an error chain. Pinned by
/// `a_live_session_never_prints_its_socket_path`, because a later
/// `#[derive(Debug)]` here would be a silent regression.
pub struct Live(ReconnectingClient);

impl fmt::Debug for Live {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Live").field("socket", &Redacted).finish()
    }
}

impl Live {
    /// Wrap a connected client.
    #[must_use]
    pub fn new(client: ReconnectingClient) -> Self {
        Self(client)
    }

    /// What the client's supervisor is doing right now.
    #[must_use]
    pub fn link(&self) -> LinkState {
        self.0.link()
    }

    /// Read this dog's own `[<name>]` section of `dogs.toml`, as TOML text.
    ///
    /// Empty when `dogs.toml` has no such section, which is the ordinary
    /// case for a dog running on its defaults.
    ///
    /// # Errors
    /// [`Error::Connect`] or [`Error::Request`] if the shepherd cannot be
    /// reached or refuses, and [`Error::Unexpected`] if it answers with
    /// something other than a dog section.
    pub async fn section(&self, name: &str) -> Result<String, Error> {
        let asked = Request::DogConfig {
            name: name.to_owned(),
        };
        match self.0.request(asked).await? {
            Response::DogSection { toml } => Ok(toml.as_str().to_owned()),
            other => Err(unexpected("a DogSection", &other)),
        }
    }

    /// Every supervised entry, dogs included.
    ///
    /// # Errors
    /// As [`Self::section`].
    pub async fn flock(&self) -> Result<Vec<ProcessInfo>, Error> {
        match self.0.request(Request::ListFlock).await? {
            Response::Flock(sheep) => Ok(sheep),
            other => Err(unexpected("a Flock", &other)),
        }
    }

    /// What the machine the flock runs on is doing right now.
    ///
    /// `None` where the shepherd cannot sample its own host at all, which is
    /// a state for a caller to render rather than an error. A shepherd too
    /// old to know the verb is the other case and is an error: `HostUsage`
    /// arrived with protocol 9, so an older one refuses it by name and
    /// `/system` says so instead of drawing an empty embed.
    ///
    /// # Errors
    /// As [`Self::section`].
    pub async fn host_usage(&self) -> Result<Option<HostUsage>, Error> {
        match self.0.request(Request::HostUsage).await? {
            Response::HostUsage(usage) => Ok(usage),
            other => Err(unexpected("a HostUsage", &other)),
        }
    }

    /// Act on matching sheep, and describe what happened in one sentence
    /// for a person to read, not a caller that wants structured data back.
    ///
    /// See the module doc for the verb table and for why `selector` goes
    /// unused on [`Verb::Save`]'s path.
    ///
    /// # Errors
    /// As [`Self::section`].
    pub async fn act(&self, verb: Verb, selector: SelectorSpec) -> Result<String, Error> {
        match verb {
            Verb::Start => self.restart(selector, "started").await,
            Verb::Restart => self.restart(selector, "restarted").await,
            Verb::Reload => match self.0.request(Request::Reload { selector }).await? {
                Response::Reloading { accepted, refused } => {
                    Ok(walked("reloading", &accepted, &refused))
                }
                other => Err(unexpected("a Reloading", &other)),
            },
            Verb::Stop => match self.0.request(Request::Stop { selector }).await? {
                Response::Stopped(sheep) => Ok(format!("stopped {}", name_list(&sheep))),
                other => Err(unexpected("a Stopped", &other)),
            },
            Verb::Delete => {
                // `Deleted` answers with ids alone, so the name in the
                // reply has to come from what was asked rather than from
                // what came back. See the module doc.
                let named = describe_selector(&selector);
                match self.0.request(Request::Delete { selector }).await? {
                    Response::Deleted(_) => Ok(format!("deleted {named}")),
                    other => Err(unexpected("a Deleted", &other)),
                }
            }
            // The only place `Request::Flush` is built, and only this verb
            // reaches it: see the module doc.
            Verb::Flush => match self.0.request(Request::Flush { selector }).await? {
                Response::Flushed(sheep) => Ok(format!("flushed {}", name_list(&sheep))),
                other => Err(unexpected("a Flushed", &other)),
            },
            Verb::Reopen => match self.0.request(Request::Reopen { selector }).await? {
                Response::Reopened(sheep) => Ok(format!("reopened {}", name_list(&sheep))),
                other => Err(unexpected("a Reopened", &other)),
            },
            Verb::Save => match self.0.request(Request::SaveRoll).await? {
                Response::RollSaved { path, apps } => Ok(format!("saved {apps} apps to {path}")),
                other => Err(unexpected("a RollSaved", &other)),
            },
        }
    }

    /// [`Verb::Start`] and [`Verb::Restart`] both ask [`Request::Restart`]
    /// and differ only in the word the reply uses: see the module doc for
    /// why `Start` does not ask `Request::Start`.
    async fn restart(&self, selector: SelectorSpec, verb_word: &str) -> Result<String, Error> {
        match self.0.request(Request::Restart { selector }).await? {
            Response::Restarted { accepted, refused } => Ok(walked(verb_word, &accepted, &refused)),
            other => Err(unexpected("a Restarted", &other)),
        }
    }

    /// Wait until the client's supervisor is on a connection again, for
    /// at most `budget`, returning at once when it already is.
    ///
    /// What a caller needing a fresh [`EventStream`] after a handover
    /// waits on, and the only reason this crate looks at the link other
    /// than to notice a refusal. An [`EventStream`] belongs to one
    /// connection generation and is not re-armed across a reconnect, so a
    /// subscription ending is how this dog learns the shepherd handed
    /// over; resubscribing immediately would fail at once against the
    /// dead generation, and sleeping a fixed interval instead would mean
    /// a silent hole in the log channel for as long as that interval.
    ///
    /// An `Ok` is where to try again, never a promise the next subscribe
    /// works: the supervisor can still report `Connected` for the moment
    /// between a socket dying and it waking, and a fresh connection can
    /// die immediately after. The caller asks, and comes back here if it
    /// has budget left.
    ///
    /// # Errors
    /// [`LinkLost::Refused`] when a successor refused on protocol-version
    /// skew, which no later wait can fix, and [`LinkLost::Budget`] when
    /// `budget` ran out with the supervisor still dialling.
    pub async fn connected_within(&self, budget: core::time::Duration) -> Result<(), LinkLost> {
        self.0.connected_within(budget).await
    }

    /// Subscribe this connection to bus topics.
    ///
    /// # Errors
    /// [`Error::Connect`] or [`Error::Request`] if the shepherd cannot be
    /// reached or refuses.
    pub async fn subscribe(&self, topics: Vec<String>) -> Result<EventStream, Error> {
        Ok(self.0.subscribe(topics).await?)
    }
}

/// The shepherd answered something this dog cannot use.
fn unexpected(asked: &str, got: &Response) -> Error {
    Error::Unexpected {
        asked: asked.to_owned(),
        got: named(got),
    }
}

/// Name a response without printing its body.
///
/// Hand-named per variant this dog asks for, rather than `{got:?}`, because
/// a listing variant carries its whole listing and a `DogSection` carries a
/// section that can hold a webhook credential; naming the shape keeps both
/// out of an error message.
fn named(response: &Response) -> String {
    match response {
        Response::Pong => "Pong".to_owned(),
        Response::Flock(sheep) => format!("a Flock of {}", sheep.len()),
        Response::HostUsage(_) => "a HostUsage".to_owned(),
        Response::Described(sheep) => format!("a Described of {}", sheep.len()),
        Response::DogSection { .. } => "a DogSection".to_owned(),
        Response::Stopped(sheep) => format!("a Stopped of {}", sheep.len()),
        Response::Restarted { accepted, .. } => format!("a Restarted of {}", accepted.len()),
        Response::Reloading { accepted, .. } => format!("a Reloading of {}", accepted.len()),
        Response::Deleted(ids) => format!("a Deleted of {}", ids.len()),
        Response::Flushed(sheep) => format!("a Flushed of {}", sheep.len()),
        Response::Reopened(sheep) => format!("a Reopened of {}", sheep.len()),
        Response::RollSaved { apps, .. } => format!("a RollSaved of {apps}"),
        // `Response` is `#[non_exhaustive]`, so a variant the protocol
        // grows after this dog was written reaches this arm, which is
        // exactly the one worth naming. Truncated, because some of them
        // are listings.
        other => format!("{other:?}").chars().take(60).collect(),
    }
}

/// Render the sheep a walk reached, and the ones it refused when there are
/// any.
///
/// Naming only the accepted set would hide a refusal from the one place an
/// operator would see it, so the sentence names both once `refused` is not
/// empty.
fn walked(verb_word: &str, accepted: &[ProcessInfo], refused: &[SheepRefusal]) -> String {
    let names = name_list(accepted);
    if refused.is_empty() {
        format!("{verb_word} {names}")
    } else {
        let reasons: Vec<String> = refused
            .iter()
            .map(|refusal| format!("{}: {}", refusal.name, refusal.reason))
            .collect();
        format!("{verb_word} {names}, refused {}", reasons.join(", "))
    }
}

/// A comma-joined list of sheep names, for a reply sentence.
fn name_list(sheep: &[ProcessInfo]) -> String {
    if sheep.is_empty() {
        return "no sheep".to_owned();
    }
    sheep
        .iter()
        .map(|info| info.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A human phrase for what a selector asked for, used only where the
/// shepherd's own reply carries no name to report instead: [`Request::Delete`]
/// answers with ids alone.
fn describe_selector(selector: &SelectorSpec) -> String {
    match selector {
        SelectorSpec::All => "all sheep".to_owned(),
        SelectorSpec::Id(id) => id.to_string(),
        SelectorSpec::Name(name) | SelectorSpec::Fold(name) => name.clone(),
        SelectorSpec::Regex(pattern) => pattern.clone(),
        SelectorSpec::Instance { name, slot } => format!("{name}.{slot}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{sample, test_live};
    use shep_client::{ReconnectingClient, testing};

    #[tokio::test]
    async fn start_asks_a_restart_and_still_says_started() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Restart {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Restarted {
            accepted: vec![sample("web")],
            refused: Vec::new(),
        });
        let reply = live
            .act(Verb::Start, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "started web");
    }

    #[tokio::test]
    async fn restart_names_the_sheep_in_its_reply() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Restart {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Restarted {
            accepted: vec![sample("web")],
            refused: Vec::new(),
        });
        let reply = live
            .act(Verb::Restart, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "restarted web");
    }

    #[tokio::test]
    async fn a_refused_restart_names_the_refusal_too() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Restart {
            selector: SelectorSpec::Fold("prod".to_owned()),
        })
        .answer(Response::Restarted {
            accepted: vec![sample("web")],
            refused: vec![SheepRefusal::new("worker", "exceeded restart budget")],
        });
        let reply = live
            .act(Verb::Restart, SelectorSpec::Fold("prod".to_owned()))
            .await
            .expect("ok");
        assert_eq!(
            reply,
            "restarted web, refused worker: exceeded restart budget"
        );
    }

    #[tokio::test]
    async fn reload_says_reloading_not_reloaded() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Reload {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Reloading {
            accepted: vec![sample("web")],
            refused: Vec::new(),
        });
        let reply = live
            .act(Verb::Reload, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "reloading web");
    }

    #[tokio::test]
    async fn stop_names_the_sheep_it_stopped() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Stop {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Stopped(vec![sample("web")]));
        let reply = live
            .act(Verb::Stop, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "stopped web");
    }

    #[tokio::test]
    async fn delete_names_what_was_asked_since_ids_carry_no_name() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Delete {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Deleted(vec![7]));
        let reply = live
            .act(Verb::Delete, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "deleted web");
    }

    #[tokio::test]
    async fn flush_names_the_sheep_it_flushed() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Flush {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Flushed(vec![sample("web")]));
        let reply = live
            .act(Verb::Flush, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "flushed web");
    }

    #[tokio::test]
    async fn reopen_names_the_sheep_it_reopened() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Reopen {
            selector: SelectorSpec::Name("web".to_owned()),
        })
        .answer(Response::Reopened(vec![sample("web")]));
        let reply = live
            .act(Verb::Reopen, SelectorSpec::Name("web".to_owned()))
            .await
            .expect("ok");
        assert_eq!(reply, "reopened web");
    }

    #[tokio::test]
    async fn save_names_the_roll_it_wrote_not_a_sheep() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::SaveRoll).answer(Response::RollSaved {
            path: "roll.toml".to_owned(),
            apps: 3,
        });
        // The selector is unused on this path; any value proves that.
        let reply = live.act(Verb::Save, SelectorSpec::All).await.expect("ok");
        assert_eq!(reply, "saved 3 apps to roll.toml");
    }

    #[tokio::test]
    async fn host_usage_hands_back_what_the_shepherd_sampled() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::HostUsage)
            .answer(Response::HostUsage(Some(HostUsage {
                cpu_percent: Some(12.5),
                memory_used_bytes: 1024,
                memory_total_bytes: 4096,
                disk_bytes_per_second: Some((1, 2)),
                network_bytes_per_second: None,
            })));
        let usage = live.host_usage().await.expect("ok").expect("sampled");
        assert_eq!(usage.cpu_percent, Some(12.5));
        assert_eq!(usage.memory_used_bytes, 1024);
        assert_eq!(usage.memory_total_bytes, 4096);
        assert_eq!(usage.disk_bytes_per_second, Some((1, 2)));
        assert_eq!(usage.network_bytes_per_second, None);
    }

    /// A host the shepherd cannot sample answers `None` rather than an
    /// error, so `/system` can say it is not sampling instead of failing
    /// the whole command.
    #[tokio::test]
    async fn an_unsampled_host_is_none_rather_than_an_error() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::HostUsage)
            .answer(Response::HostUsage(None));
        assert_eq!(live.host_usage().await.expect("ok"), None);
    }

    #[tokio::test]
    async fn a_wrong_shaped_answer_names_what_was_asked_and_what_came_back() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::ListFlock).answer(Response::Pong);
        let err = live.flock().await.expect_err("refused").to_string();
        assert!(err.contains("a Flock"), "{err}");
        assert!(err.contains("Pong"), "{err}");
    }

    #[tokio::test]
    async fn section_reads_the_dog_config_toml() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::DogConfig {
            name: "discord".to_owned(),
        })
        .answer(Response::DogSection {
            toml: "flush = \"1s\"\n".to_owned().into(),
        });
        let toml = live.section("discord").await.expect("ok");
        assert_eq!(toml, "flush = \"1s\"\n");
    }

    #[tokio::test]
    async fn subscribe_hands_back_an_event_stream() {
        let (live, mut fake) = test_live().await;
        fake.expect(Request::Subscribe {
            topics: vec!["process.*".to_owned()],
        })
        .answer(Response::Subscribed);
        live.subscribe(vec!["process.*".to_owned()])
            .await
            .expect("ok");
    }

    /// `Live`'s `Debug` is written rather than derived so a socket path
    /// under somebody's home directory cannot reach a log or a bug report.
    /// Proven against a real `ReconnectingClient` over a real socket,
    /// because the thing being guarded against is the derived impl, and a
    /// fake that held no client could not tell the two apart.
    #[tokio::test]
    async fn a_live_session_never_prints_its_socket_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = testing::control_address(dir.path());
        let _envelopes =
            testing::fake_daemon_answering_with_ack(&socket, testing::sample_ack(), |_request| {
                Response::Pong
            })
            .await;
        let client = ReconnectingClient::connect(&socket)
            .await
            .expect("test dial");
        let shown = format!("{:?}", Live::new(client));

        assert_eq!(shown, "Live { socket: <redacted> }");
        assert!(
            !shown.contains(&socket.display().to_string()),
            "the socket path leaked: {shown}"
        );
    }
}
