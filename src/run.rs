//! Being a shep dog: the run loop, and the two names this dog answers to.
//!
//! `main` is about starting a process, answering the probe and parsing
//! arguments; this module is about being one. [`run`] holds the socket
//! open, rereads this dog's own `dogs.toml` section on a timer, spawns the
//! gateway once, starts the monitor once, and hands the bus subscription
//! to [`crate::session::stream_once`] for as long as it lasts. Three
//! things in here are fatal: a signal, a refused handshake, and a section
//! of `dogs.toml` carrying a value this dog will not accept. The Discord
//! clients all of that writes through are [`crate::wiring::Wiring`], built
//! here on the first cycle that resolves a config.
//!
//! # How it learns its own name
//!
//! Two names, from two places, and conflating them is a mistake worth its
//! own type. See [`Identity`].
//!
//! The handshake name is what goes in the `Hello` frame, and it comes from
//! `$SHEP_DOG_NAME` and from nowhere else. shep sets it in the environment
//! it spawns an adopted dog with, and the daemon records a handshake only
//! for a connection that carries one. A dog that connects anonymously
//! serves every request correctly and is still rendered `silent`, restarted
//! once, and then declared stale and left down. There is nothing to guess
//! at here, and guessing would be worse than not knowing: the name is also
//! how the daemon decides which dog to act on when it refuses a handshake,
//! so a borrowed name restarts somebody else's dog. No `$SHEP_DOG_NAME`
//! means no shepherd spawned this process, and it connects without a name
//! at all.
//!
//! The config name is the `[<name>]` key the settings live under in
//! `dogs.toml`. Getting that one wrong is silent in its own way: the daemon
//! answers `DogConfig` for a name nobody adopted with an empty section,
//! which is byte for byte what a dog running on its defaults gets. It is
//! the handshake name whenever there is one, and [`DEFAULT_NAME`] when
//! there is not, because somebody running this binary by hand still wants
//! their `dogs.toml` read.

use std::{path::Path, process::ExitCode, sync::Arc, time::Duration};

use shep_client::{ConnectError, LinkState, ReconnectingClient};

use crate::{
    bot, config,
    error::Error,
    session,
    shepherd::Live,
    stop::{Interrupted, Stop, wait},
    wiring::Wiring,
};

/// The `[<name>]` section to read when `$SHEP_DOG_NAME` is unset, which
/// means nothing adopted this process and somebody is running the binary by
/// hand.
///
/// A config-section default only. It is never announced in the handshake:
/// see [`Identity`] for why the two names part company here rather than
/// sharing one fallback.
const DEFAULT_NAME: &str = "discord";

/// The message printed when nothing adopted this process.
///
/// A function rather than an inline `eprintln!` so the dash check can reach
/// the text without running `main`, which would call `probe` and read the
/// real process environment.
fn unadopted_message(section: &str) -> String {
    format!(
        "shep-discord: $SHEP_DOG_NAME is not set, so nothing adopted this process. It will \
         connect without naming itself, which the shepherd does not count as a handshake, \
         and read [{section}] in dogs.toml once there is a socket to read it from."
    )
}

/// The message printed when this dog's own section of `dogs.toml` could
/// not be read or could not be resolved into a [`config::Config`].
///
/// Names the section, which the bare error does not. The likeliest
/// failure on a first run is an operator who adopted this dog under one
/// name and wrote `[discord]` in `dogs.toml` under another: the shepherd
/// serves an empty section for the name nobody wrote, resolving it fails
/// on `token is required`, and that sentence alone sends the reader to
/// look at a `token` they have already set correctly. The section name is
/// the whole diagnosis.
fn config_failed_message(section: &str, err: &Error) -> String {
    format!("shep-discord: [{section}] in dogs.toml: {err}")
}

/// How many cycles a repeated complaint is muted for before it is said
/// again.
///
/// Counted in cycles rather than in time so it tracks
/// [`RECHECK_INTERVAL`] instead of drifting away from it; 120 of them is
/// an hour at thirty seconds.
///
/// What matters is that it is not infinity, which is what it used to be.
/// Muting a repeat forever is defensible for a complaint that would
/// otherwise print 2,880 identical lines a day, and indefensible for the
/// one thing this loop stays up for: a dog nobody has configured yet is
/// deliberately left running, and a dog that said why once and then never
/// again is indistinguishable from a working one by the time anybody
/// looks. The shepherd reports it online, `shep bleats` shows nothing at
/// all, and the line that explained it scrolled past hours ago.
///
/// Borrowed from shep-deploy's `RESAY`, which is the same number for the
/// same reason.
const RESAY: u32 = 120;

/// The last complaint this loop printed, and how many cycles it has been
/// muted for since.
struct Complaint {
    /// The line as it was printed.
    line: String,
    /// Cycles since it was last printed.
    muted: u32,
}

/// Whether this cycle's config outcome is worth printing, threading the
/// last complaint through `last` rather than a process-global, the same
/// shape and for the same reason as `session::warn_once`.
///
/// `None` is a cycle that read the config fine, and it clears the state,
/// so a failure that comes back after a good read is announced again.
/// `Some` prints when the sentence differs from the last one printed, so
/// an operator who fixes one mistake in `dogs.toml` and makes a different
/// one hears about the new one; a repeat of the same sentence prints once
/// more every [`RESAY`] cycles, and says nothing in between.
fn worth_saying(message: Option<&str>, last: &mut Option<Complaint>) -> bool {
    let Some(message) = message else {
        *last = None;
        return false;
    };

    if let Some(seen) = last.as_mut()
        && seen.line == message
    {
        seen.muted += 1;
        if seen.muted < RESAY {
            return false;
        }
    }

    *last = Some(Complaint {
        line: message.to_owned(),
        muted: 0,
    });
    true
}

/// The message printed when the gateway task this loop spawned has ended
/// on its own, distinguishing a panic from an ordinary return.
///
/// A pure function of the spawned task's own `JoinHandle` result, the same
/// reason every other message here is a function rather than an inline
/// `eprintln!`: it lets a test drive both branches with a real, spawned
/// task and no gateway behind either.
fn gateway_ended_message(outcome: &Result<(), tokio::task::JoinError>) -> String {
    match outcome {
        Ok(()) => "shep-discord: the gateway task ended on its own; starting a fresh one on the \
                    next cycle."
            .to_owned(),
        Err(err) if err.is_panic() => format!(
            "shep-discord: the gateway task panicked: {err}. Starting a fresh one on the next \
             cycle rather than leaving Discord silently unanswered for the rest of this process."
        ),
        Err(err) => format!(
            "shep-discord: the gateway task was cancelled: {err}. Starting a fresh one on the \
             next cycle."
        ),
    }
}

/// The two names this dog needs, and the two different places they come
/// from.
///
/// Separate fields rather than one string, because they answer different
/// questions and only one of them may be guessed at. The module docs have
/// the argument; this type is what stops the code drifting back to one
/// name for both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// What to announce in the `Hello` frame, or `None` for a process no
    /// shepherd spawned.
    ///
    /// Never falls back to [`DEFAULT_NAME`]. A name the daemon did not hand
    /// out is a name it will act on anyway: a refused handshake is recorded
    /// against whatever the frame said, so an invented one asks the daemon
    /// to restart a dog that is running perfectly well.
    handshake: Option<String>,
    /// The `[<name>]` section to read out of `dogs.toml`.
    section: String,
}

impl Identity {
    /// Read both names out of the environment.
    ///
    /// Takes a lookup rather than reading [`std::env::var`] itself. The
    /// environment is a process-wide mutable global, and a test that sets
    /// one variable to check the absent case is a test that races every
    /// other test in the binary.
    ///
    /// An empty `$SHEP_DOG_NAME` reads as unset. It cannot be a real dog:
    /// `[]` is not a section anybody can write, and an empty name in a
    /// `Hello` frame is a handshake the daemon cannot attribute either.
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let handshake = env("SHEP_DOG_NAME").filter(|name| !name.is_empty());
        let section = handshake.clone().unwrap_or_else(|| DEFAULT_NAME.to_owned());
        Self { handshake, section }
    }
}

/// Connect, announcing the handshake name when there is one.
///
/// The name is settled once, before the loop starts, rather than looked up
/// per connection: it comes out of the environment shep spawned this
/// process with, and a shepherd that restarted underneath the dog did not
/// reach into that environment and rewrite it.
async fn connect(socket: &Path, identity: &Identity) -> Result<Live, Error> {
    let client = match &identity.handshake {
        Some(name) => ReconnectingClient::connect_as_dog(socket, name).await?,
        None => ReconnectingClient::connect(socket).await?,
    };
    Ok(Live::new(client))
}

/// The message printed when the shepherd refuses this dog's handshake.
///
/// A function rather than an inline `eprintln!` so the dash check can reach
/// the text directly, the same reason [`unadopted_message`] is one.
fn refused_message(daemon_version: Option<&str>, message: &str) -> String {
    format!(
        "shep-discord: the shepherd refused this dog's handshake, and no amount of \
         reconnecting fixes a protocol-version skew. The shepherd reports {}, and said: \
         {message}. Exiting so it can restart this dog from disk.",
        daemon_version.unwrap_or("no version")
    )
}

/// Say why this dog is stopping, and hand back the code to stop with.
///
/// A refused handshake is protocol-version skew, and it is the one failure
/// in here that waiting cannot fix: the daemon that refused is the only
/// party that can, every later request on that connection fails, and the
/// client's own supervisor has already given up rather than retrying it.
///
/// Exiting is also what makes the refusal actionable. The shepherd restarts
/// a dog from its recorded path, and a skew usually means the binary at
/// that path has already been replaced by the one that matches, so the
/// restart is the fix rather than a retry of the same mistake.
fn refused(daemon_version: Option<&str>, message: &str) -> ExitCode {
    eprintln!("{}", refused_message(daemon_version, message));
    ExitCode::FAILURE
}

/// The exit code a refused `dogs.toml` ends this process on.
///
/// shep's own number for the cause: `invalid_config`, `4`, which
/// `shep-cli`'s `ExitCode` enum documents as "a Flockfile or daemon
/// config failed validation" and the daemon answers as an `RpcErrorCode`
/// for a dog field it will not take. A section this dog will not take is
/// the same cause seen from the other end of the same wire.
///
/// Read rather than written, which is the whole test. shep assigns `0`
/// through `13` and nothing above, and `12` and `13` are its own
/// `VersionSkew` and `Unsupported` rather than a range held open for
/// dogs: no part of that taxonomy is reserved for a dog to fill in. So
/// the choice here was never between a documented code and a new one of
/// this crate's own. It was between shep's specific number for this
/// cause and its generic `Failure`, `1`, and a bare `1` in the `EXIT`
/// column tells an operator only that something stopped the dog.
///
/// Written out rather than imported, for the reason `PROTOCOL_MISMATCH`
/// is in shep-pm/shep-discord#4: the taxonomy lives in `shep-cli`, a
/// binary crate with nothing to import from, so the number itself is the
/// contract.
const INVALID_CONFIG: u8 = 4;

/// The message printed when this dog is stopping because its own section
/// names a value it will not accept.
///
/// A function rather than an inline `eprintln!` so the dash check can
/// reach the text, the same reason [`refused_message`] is one. It says
/// more than [`config_failed_message`] does on purpose: an operator
/// reading it is about to watch this dog crash loop into `Errored`, and
/// the sentence has to carry both why that is happening and what ends it.
fn misconfigured_message(section: &str, err: &Error) -> String {
    format!(
        "shep-discord: [{section}] in dogs.toml: {err}. Nothing about that clears on its own,          so every retry would be the same failure. Exiting rather than staying up: a dog that          kept answering the shepherd's handshake while answering Discord never would be          reported online on every column a listing has, and the only evidence would be this          line. Fix the value and run shep restart {section}."
    )
}

/// Say why this dog is stopping over a config, or say nothing much and
/// let it carry on, and hand back the code to stop with when it is
/// stopping.
///
/// This is the whole of the change that made a wrong config fatal, and
/// the reason [`Error::Unconfigured`] exists beside [`Error::Config`].
/// Three things reach this arm and only one of them is fatal:
///
/// - **A section carrying a value this dog refuses** ([`Error::Config`]):
///   a `buffer_lines = 0`, a `guild_id = 0`, a duration shep's grammar
///   will not parse, a key this dog does not know, text that is not TOML.
///   Nothing changes until an operator edits the file, so staying up
///   means an infinite run of identical failures with the dog reported
///   online throughout, which is exactly the anti-pattern shep's own
///   `/docs/writing-a-dog` names. It exits, the same argument
///   [`refused`] makes for a refused handshake.
/// - **A section nobody has filled in yet** ([`Error::Unconfigured`]): no
///   `token`, no `guild_id`. This is every freshly adopted dog, because
///   `shep adopt` vets, registers, enables and starts in one command, so
///   exiting here would make this dog impossible to adopt at all. It
///   stays up and says so on [`RESAY`]'s cadence.
/// - **A shepherd that could not answer** ([`Error::Connect`],
///   [`Error::Request`], [`Error::Unexpected`]): a daemon mid restart or
///   mid handover. It clears on its own, so it stays up too.
///
/// The cost of the first one is self healing, and it is worth naming.
/// Before this, a `dogs.toml` fixed after the fact was picked up on the
/// next cycle, since this loop rereads its section every pass. Now a dog
/// that exited needs `shep restart <name>` once the typo is fixed,
/// because the crash loop will have spent its restart budget and left it
/// `Errored`. That trade is deliberate: an honest status an operator can
/// see beats a silent recovery they cannot.
fn on_config_failure(section: &str, err: &Error, last: &mut Option<Complaint>) -> Option<ExitCode> {
    if matches!(err, Error::Config(_)) {
        eprintln!("{}", misconfigured_message(section, err));
        return Some(ExitCode::from(INVALID_CONFIG));
    }

    let message = config_failed_message(section, err);
    if worth_saying(Some(&message), last) {
        eprintln!("{message}");
    }
    None
}

/// How often the run loop rechecks [`Live::link`] and rereads its own
/// `dogs.toml` section.
///
/// Fixed rather than read from `[discord]`, because nothing in that section
/// governs this. The Discord gateway and the shepherd's own event bus are
/// both wired in now, and both are waited on rather than polled, but this
/// loop still wakes on the timer: rereading `dogs.toml` is what it is for,
/// and the shepherd announces a config change on no topic this dog
/// subscribes to. Thirty seconds is short enough that a handshake refused
/// after the fact, or an edited config, is noticed promptly, and long
/// enough not to ask the shepherd for a section many times a second.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(30);

/// The run loop.
///
/// A failed cycle is printed and retried on the next interval, because
/// the shepherd restarting underneath a dog is ordinary rather than
/// exceptional, and exiting would ask the supervisor to restart this
/// process for a condition that resolves itself on its own. Three things
/// are not that, and end the process instead: a signal, a refused
/// handshake, and a `dogs.toml` section this dog will not accept. See
/// [`on_config_failure`] for the last of them, which is the only one that
/// has to tell two failures apart to get it right.
///
/// There is no signal handling beyond `ctrl_c`, which is the clean-exit
/// path for an operator running this binary in a terminal; see
/// [`crate::stop`] for why the shepherd owns everything else.
///
/// # Running the gateway alongside the stream
///
/// [`crate::session::stream_once`] does not return until its bus subscription
/// ends or `stop` resolves, so it stands in for this whole loop's ordinary
/// work for as long as it runs. A gateway client started after this loop,
/// the way this function reads top to bottom, would never get a turn:
/// [`crate::bot::run`] has the same shape, a loop that does not return until
/// `stop` resolves either. Two functions shaped like that cannot run one
/// after the other in the same task and both make progress; they have to
/// run in two.
///
/// This loop is left as it was and [`crate::bot::run`] is spawned as its own
/// task, sharing this loop's own [`Live`] (behind an `Arc`, since a
/// spawned task needs `'static` and cannot borrow this loop's stack) and
/// watching a clone of the same [`Stop`], rather than the two other
/// shapes considered:
///
/// - **One `tokio::select!` racing both loops in this one task.** Rejected
///   because `select!` drops whichever branch did not finish first, and
///   both branches here are meant to run forever until `stop` resolves;
///   racing them would tear down whichever happened to still be mid
///   request the moment the other's loop iteration completed, which is
///   not a stop this dog was asked for.
/// - **A second, fully independent shepherd connection for the gateway
///   side,** each with its own reconnect loop, rather than sharing this
///   one. Rejected as needless: [`Live`] wraps a [`ReconnectingClient`],
///   which is itself already safe to share behind `&self`, and a second
///   socket to the same shepherd would double the handshake traffic for
///   no isolation this dog actually needs, since a refused handshake on
///   either connection means the same thing and this loop already exits
///   the whole process for it.
///
/// The gateway is started once, the first time this loop resolves a
/// config with a `token` and `guild_id` in it, rather than restarted on
/// every later reread the way the streaming side is: reconnecting the
/// gateway every [`RECHECK_INTERVAL`] would drop and reopen it for no
/// reason on almost every cycle, since a token or guild rarely changes,
/// and rebuilding [`crate::bot::interaction::Handler`] on a live reload of
/// `dogs.toml` is a live-reload feature this task was not asked to add.
/// If the token or guild does change later, this dog answers `/system`
/// under the old one until the shepherd restarts it, the same way an
/// operator's other config edits already wait for a restart to take
/// effect anywhere sampling happens once at startup.
///
/// [`Wiring`] is built once on that same cycle and for the same reason,
/// so a changed `token` or `monitor_channel` waits for the same restart
/// on the streaming and monitor sides as it already does on the gateway
/// one. What it buys is the rate-limit state those clients carry; see
/// [`Wiring`] itself.
pub async fn run(socket: &Path, identity: &Identity) -> ExitCode {
    let mut stop = Stop::on_ctrl_c();
    let mut session: Option<Arc<Live>> = None;
    // Owned here rather than behind a process-global inside `stream_once`,
    // the same reason `stop` and `session` are: it is per-cycle state this
    // loop is the only caller of, and a static hides that state from every
    // test that would otherwise exercise it. See `session::warn_once`.
    let mut unresolved_warned = false;
    // One monitor for the whole process, built before the loop so a
    // reconnect or a config reread keeps the message ids it has already
    // cached: losing them would make the next refresh post a second embed
    // beside every one already in the channel. Shared with the gateway
    // task through `bot::command::State`.
    let monitor = Arc::new(bot::monitor::Monitor::new());
    // The last config complaint printed, so an unfixed `dogs.toml` says
    // its piece once an hour rather than on every cycle. See
    // `worth_saying`.
    let mut last_config_complaint: Option<Complaint> = None;
    // Whether the boot start below has already had its one turn: it gets
    // exactly one, and the comment at that call site has the why.
    let mut monitor_started = false;
    // `Some` while the gateway task is running; see the doc above for why
    // it starts once rather than on every cycle. Cleared back to `None`
    // the cycle after the task finishes, panic or not, so a gateway that
    // panicked does not leave this dog half dead: Discord silently
    // unanswered while the socket connection to the shepherd, and this
    // loop's own reads of `dogs.toml`, keep running as if nothing had
    // happened. Before the check below existed, `is_none()` was the only
    // read of this field, so a `Some` set once and never cleared stayed
    // `Some` whether the task behind it was alive or long since dead.
    let mut gateway: Option<tokio::task::JoinHandle<()>> = None;
    // `Some` from the first cycle that resolves a config onward, and
    // never rebuilt after that; see `Wiring` for why one of each is worth
    // holding, and `run`'s own doc for the restart this costs an operator
    // who edits `token` or `monitor_channel`.
    let mut wiring: Option<Wiring> = None;

    if identity.handshake.is_none() {
        // Once, before the loop, rather than per connection: the answer
        // cannot change while this process runs, and a dog whose socket is
        // not up yet would otherwise print it on every retry.
        //
        // Loudly, because the two things it means are worth telling apart.
        // Run by hand it is expected. Under a shepherd it means something
        // stripped the environment between `shep adopt` and this process,
        // and the daemon is about to call this dog silent and stop
        // restarting it.
        eprintln!("{}", unadopted_message(&identity.section));
    }

    loop {
        // Checked every cycle, before anything else: a finished handle
        // means the task behind it is gone, panic or ordinary return
        // alike, and `gateway.is_none()` further down is the only thing
        // that ever starts a new one. Left `Some`, this dog would answer
        // Discord never again for the rest of the process while looking
        // otherwise alive.
        if let Some(handle) = gateway.take_if(|handle| handle.is_finished()) {
            eprintln!("{}", gateway_ended_message(&handle.await));
        }

        if session.is_none() {
            match connect(socket, identity).await {
                Ok(live) => session = Some(Arc::new(live)),
                Err(Error::Connect(ConnectError::ProtocolMismatch {
                    daemon_version,
                    message,
                    ..
                })) => return refused(daemon_version.as_deref(), &message),
                Err(err) => eprintln!("shep-discord: {err}"),
            }
        }

        if let Some(live) = &session {
            // Before this cycle's work rather than after a failed one: a
            // refused link answers every request with the same closed
            // connection, and a failed read would say nothing about why.
            if let LinkState::Refused {
                daemon_version,
                message,
            } = live.link()
            {
                return refused(daemon_version.as_deref(), &message);
            }

            match live
                .section(&identity.section)
                .await
                .and_then(|toml| config::Config::from_toml(&toml))
            {
                Ok(config) => {
                    worth_saying(None, &mut last_config_complaint);
                    let config = Arc::new(config);
                    let wiring = wiring.get_or_insert_with(|| Wiring::new(&config, &monitor));

                    if gateway.is_none() {
                        let state = bot::command::State {
                            live: Arc::clone(live),
                            config: Arc::clone(&config),
                            monitor: Arc::clone(&monitor),
                            board: wiring.board.clone(),
                        };
                        gateway = Some(tokio::spawn(bot::run(
                            Arc::clone(&config),
                            state,
                            stop.clone(),
                        )));
                    }

                    // Started once, on the first cycle that resolves a
                    // config asking for it, rather than on every cycle:
                    // this loop rereads the same `dogs.toml` every
                    // `RECHECK_INTERVAL`, and starting again there would
                    // restart the monitor underneath an operator who had
                    // just run `/monitor stop`. `monitor_interval` is what
                    // says "run this from boot"; `/monitor start` is the
                    // runtime override, and it says in its own reply that
                    // it is not written anywhere.
                    if !monitor_started {
                        // The ATTEMPT is what is recorded, not its
                        // outcome. A start that found one already running
                        // (an operator quicker with `/monitor start` than
                        // this loop was to resolve a config) has still had
                        // its turn, so recording only a successful start
                        // would leave this flag false and let the next
                        // cycle start a monitor the operator may have
                        // stopped in between, which is the exact thing the
                        // flag exists to prevent.
                        monitor_started = true;
                        bot::monitor::refresh::start_from_config(
                            &monitor,
                            &config,
                            wiring.board.as_ref(),
                            live,
                        );
                    }

                    // Subscribing only when something reads the bus: the
                    // two log channels for the lines themselves, and
                    // `monitor_channel` for the `process.*` events that
                    // redraw a sheep between refreshes. This await does not
                    // return until that subscription ends or `stop`
                    // resolves, so it stands in for this cycle's ordinary
                    // work for as long as it runs; when it returns, the
                    // loop's own wait and reconnect below try again.
                    if config.log_channel.is_some()
                        || config.err_channel.is_some()
                        || config.monitor_channel.is_some()
                    {
                        let handshake = identity.handshake.as_deref();
                        if let Err(err) = session::stream_once(
                            live,
                            handshake,
                            &config,
                            &wiring.sink,
                            wiring.wired.as_ref(),
                            &mut unresolved_warned,
                            &mut stop,
                        )
                        .await
                        {
                            eprintln!("shep-discord: {err}");
                        }
                    }
                }
                Err(err) => {
                    if let Some(code) =
                        on_config_failure(&identity.section, &err, &mut last_config_complaint)
                    {
                        return code;
                    }
                }
            }
        }

        if wait(RECHECK_INTERVAL, &mut stop).await == Interrupted::Yes {
            return ExitCode::SUCCESS;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_no_dashes;

    /// Nothing this loop prints for a person carries an em dash or an en
    /// dash: a terminal that cannot render one prints a replacement
    /// character in the middle of the one message that exists to be read
    /// by somebody who is already confused. `main` carries the same check
    /// for the messages it prints before this loop is reached.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        assert_no_dashes(&unadopted_message(DEFAULT_NAME));
        assert_no_dashes(&refused_message(Some("9"), "protocol too old"));
        assert_no_dashes(&gateway_ended_message(&Ok(())));
        assert_no_dashes(&config_failed_message(
            "chatter",
            &Error::Unconfigured("token is required".to_owned()),
        ));
        assert_no_dashes(&misconfigured_message(
            "chatter",
            &Error::Config("buffer_lines must be at least 1".to_owned()),
        ));
    }

    /// The name of the section is the diagnosis for the likeliest first
    /// run failure, an operator adopting this dog as one name and writing
    /// `[discord]` under another, so it has to be in the sentence.
    #[test]
    fn a_config_complaint_names_the_section_it_read() {
        let message =
            config_failed_message("chatter", &Error::Config("token is required".to_owned()));
        assert_eq!(
            message,
            "shep-discord: [chatter] in dogs.toml: token is required"
        );
    }

    /// Once per problem, not once per cycle, and again when the problem
    /// changes. At the recheck interval an unfixed `dogs.toml` would
    /// otherwise print 2,880 identical lines a day.
    #[test]
    fn a_config_complaint_repeats_only_when_it_changes() {
        let mut last = None;
        assert!(worth_saying(Some("token is required"), &mut last));
        assert!(
            !worth_saying(Some("token is required"), &mut last),
            "the same unfixed problem says nothing on the next cycle"
        );
        assert!(
            worth_saying(Some("guild_id is required"), &mut last),
            "a different problem is a different sentence and is printed"
        );
        assert!(
            !worth_saying(None, &mut last),
            "a good read prints nothing of its own"
        );
        assert!(
            worth_saying(Some("guild_id is required"), &mut last),
            "and a problem coming back after a good read is announced again"
        );
    }

    /// The other half of the same function, and the reason it holds state
    /// at all rather than printing every cycle. A dog nobody has
    /// configured is left running on purpose, so its one explanation
    /// cannot be a line said once and never again: an hour later the
    /// shepherd still reports it online and there is nothing in the log
    /// to say why.
    ///
    /// Counted a cycle at a time rather than asserted at the boundary
    /// alone, so the silence in between is pinned as well as the line at
    /// the end of it.
    #[test]
    fn an_unconfigured_dog_says_so_again_rather_than_only_once() {
        let mut last = None;
        let complaint = "shep-discord: [discord] in dogs.toml: token is required";
        assert!(worth_saying(Some(complaint), &mut last), "the first cycle");
        for cycle in 1..RESAY {
            assert!(
                !worth_saying(Some(complaint), &mut last),
                "cycle {cycle} of {RESAY} is inside the muted window"
            );
        }
        assert!(
            worth_saying(Some(complaint), &mut last),
            "and the cycle after the window says it again"
        );
        assert!(
            !worth_saying(Some(complaint), &mut last),
            "then goes quiet for another window rather than repeating forever"
        );
    }

    /// The behavioural half, and the whole point of the change. Both
    /// halves have to be here: a version that exited on everything would
    /// pass the first loop and make this dog impossible to adopt, and a
    /// version that exited on nothing would pass the second and leave the
    /// bug exactly where it was.
    ///
    /// Driven through `Config::from_toml` against real TOML rather than
    /// hand-built errors, so it pins what an operator's `dogs.toml`
    /// actually does rather than what this test thinks the parser
    /// answers with.
    #[test]
    fn a_wrong_section_stops_this_dog_and_an_empty_one_leaves_it_running() {
        let mut last = None;
        for wrong in [
            "this is not TOML at all",
            "token = \"t\"\nguild_id = 1\nbuffer_line = 10\n",
            "token = \"t\"\nguild_id = 0\n",
            "token = \"t\"\nguild_id = 1\nlog_channel = 0\n",
            "token = \"t\"\nguild_id = 1\nflush = \"whenever\"\n",
            "token = \"t\"\nguild_id = 1\nbuffer_lines = 0\n",
        ] {
            let err = config::Config::from_toml(wrong).expect_err(wrong);
            assert!(
                on_config_failure("discord", &err, &mut last).is_some(),
                "a value this dog refuses stops it: {wrong}"
            );
        }

        for unfilled in ["", "guild_id = 1\n", "token = \"t\"\n"] {
            let err = config::Config::from_toml(unfilled).expect_err(unfilled);
            assert!(
                on_config_failure("discord", &err, &mut last).is_none(),
                "a section nobody filled in yet does not: {unfilled:?}"
            );
        }
    }

    /// The third thing that reaches the same arm, and the one easiest to
    /// sweep into the fatal half by accident. A shepherd mid restart
    /// answers every read this way, and it clears on its own, so a dog
    /// that exited for it would restart itself out of a handover it was
    /// built to sit through.
    #[test]
    fn a_shepherd_that_could_not_answer_does_not_stop_this_dog() {
        let mut last = None;
        for err in [
            Error::Request(shep_client::RequestError::Closed),
            Error::Connect(ConnectError::HandshakeClosed),
            Error::Unexpected {
                asked: "a DogConfig".to_owned(),
                got: "a Pong".to_owned(),
            },
        ] {
            assert!(
                on_config_failure("discord", &err, &mut last).is_none(),
                "{err}"
            );
        }
    }

    /// An environment holding exactly one variable, which is the only one
    /// [`Identity::from_env`] reads.
    fn only(key: &str, value: &str) -> impl Fn(&str) -> Option<String> {
        let (key, value) = (key.to_owned(), value.to_owned());
        move |asked| (asked == key).then(|| value.clone())
    }

    #[test]
    fn the_dog_takes_both_names_from_shep_dog_name() {
        let identity = Identity::from_env(only("SHEP_DOG_NAME", "chatter"));
        assert_eq!(identity.handshake.as_deref(), Some("chatter"));
        assert_eq!(identity.section, "chatter");
    }

    #[test]
    fn without_shep_dog_name_the_dog_names_itself_to_nobody() {
        let identity = Identity::from_env(|_| None);
        assert_eq!(identity.handshake, None);
        assert_eq!(identity.section, DEFAULT_NAME);
    }

    /// A panicked gateway task is told apart from one that simply
    /// returned, so an operator reading stderr knows whether Discord's own
    /// gateway dropped a shard or this dog's own code panicked. Uses a
    /// real spawned task rather than a hand-built `JoinError`: nothing in
    /// `tokio::task` constructs one directly, and spawning one is not the
    /// network call, gateway, or process this project's tests are barred
    /// from starting.
    #[tokio::test]
    async fn a_panicked_gateway_is_told_apart_from_an_ordinary_return() {
        let panicked = tokio::spawn(async { panic!("a gateway task panicking") }).await;
        assert!(panicked.is_err());
        let message = gateway_ended_message(&panicked);
        assert!(message.contains("panicked"), "{message}");

        let ended: Result<(), tokio::task::JoinError> = tokio::spawn(async {}).await;
        let message = gateway_ended_message(&ended);
        assert!(!message.contains("panicked"), "{message}");
    }
}
