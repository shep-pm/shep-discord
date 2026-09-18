//! `shep-discord`: a Discord dog for shep.
//!
//! Streams a sheep's stdout and stderr into Discord channels and, once the
//! bot side lands, answers shep's own verbs from slash commands. This file
//! is the process around that: the probe shep spawns the binary to ask, the
//! argument parser, the identity this dog announces itself with, and the
//! run loop that holds the socket open. That loop reads the
//! [`config::Config`] it parses on every cycle and, once `log_channel` or
//! `err_channel` names one, hands it to [`stream::run`] for as long as that
//! subscription lasts; the slash-command side of the bot is still a later
//! task, and nothing here opens a gateway connection for it.
//!
//! # The two questions shep asks the binary
//!
//! Before anything else runs, there is a probe. `shep adopt` spawns this
//! binary with `--version`, reads the build and the `PROTOCOL_VERSION` it
//! was compiled against, and refuses a dog below its own floor; then with
//! `--schema`, and reads a JSON Schema for the `[discord]` section, which is
//! what lookout draws a settings pane from. Both are answered by
//! [`shep_client::dogs::probe`], from [`config::Section`]'s own
//! deserialization type, on the first line of [`main`] and before this
//! process opens a socket or a file.
//!
//! Answering is optional in the contract and not optional here. A dog that
//! says nothing to `--version` is adopted with its protocol unknown, and a
//! dog that says nothing to `--schema` has no pane and a section an operator
//! hand-edits. A binary whose argument parser refused every flag it did not
//! know would answer neither: `--version` and `--schema` would reach that
//! parser and get a usage message on stderr instead, because the probe
//! never got to look at them.
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

#![forbid(unsafe_code)]

mod config;
mod error;
mod names;
mod session;
mod shepherd;
mod stop;
mod stream;
#[cfg(test)]
mod test_support;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use core::fmt;

use shep_client::{ConnectError, LinkState, ReconnectingClient, shep_core::paths::ShepPaths};

use crate::{error::Error, shepherd::Live, stop::Stop};

/// The `[<name>]` section to read when `$SHEP_DOG_NAME` is unset, which
/// means nothing adopted this process and somebody is running the binary by
/// hand.
///
/// A config-section default only. It is never announced in the handshake:
/// see [`Identity`] for why the two names part company here rather than
/// sharing one fallback.
const DEFAULT_NAME: &str = "discord";

/// Everything this binary accepts, printed when it is handed anything else.
///
/// No em dashes and no en dashes: a terminal that cannot render one prints a
/// replacement character in the middle of the one message that exists to be
/// read by somebody who is already confused.
const USAGE: &str = "\
shep-discord: a Discord dog for shep.

Usage:
  shep-discord                 Run the dog: streams log lines and answers slash
                               commands. This is what the shepherd runs after
                               `shep adopt`.
  shep-discord --print-config  Print a commented [discord] block for dogs.toml
                               naming every option and its default, then exit.
  shep-discord --version       Print the build and the protocol version it
                               speaks, then exit.
  shep-discord --schema        Print a JSON Schema for the [discord] section,
                               then exit.

The last two are what `shep adopt` asks this binary, and they are answered
only as the first argument, which is the only one shep passes.

Settings are read from `dogs.toml` over the shepherd's own socket, never
from this process's arguments. The environment supplies two things and no
more: $SHEP_HOME names the socket, and $SHEP_DOG_NAME names the dog. The
shepherd sets both when it spawns this dog.";

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

/// The message printed when neither `$HOME` nor `$SHEP_HOME` is set.
///
/// A `const` rather than an inline `eprintln!`, the same reason
/// [`unadopted_message`] is a function: it lets the dash check reach the
/// text without running `main`.
const NO_SHEP_HOME_MESSAGE: &str = "shep-discord: neither $HOME nor $SHEP_HOME is set, so there \
                                     is no shep home to find a socket in.";

/// The message printed when the Tokio runtime fails to build.
///
/// A function rather than an inline `eprintln!`, the same reason
/// [`unadopted_message`] is one.
fn cannot_start_runtime_message(err: &std::io::Error) -> String {
    format!("shep-discord: cannot start a runtime: {err}")
}

/// What this process was asked to do.
///
/// One flag, so no `clap`: a dependency that parses one argument would be
/// larger than the whole of this binary's argument surface, and that surface
/// is deliberately closed. Everything configurable is configured in
/// `dogs.toml`, where the shepherd can serve it.
///
/// shep's own two flags are not in here. `--version` and `--schema` are
/// answered and exited on by [`shep_client::dogs::probe`] before [`main`]
/// builds an [`Action`] at all, so this enum only ever sees a run that is
/// not a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Run the dog until the shepherd stops this process.
    Run,
    /// Print a commented `[discord]` block and exit.
    PrintConfig,
}

/// An argument this binary does not accept.
///
/// Carries the whole answer, [`USAGE`] included, so a caller prints one
/// thing and is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage(String);

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\n{USAGE}", self.0)
    }
}

impl core::error::Error for Usage {}

impl Action {
    /// Read the arguments, which do not include the program name.
    ///
    /// Takes an iterator rather than reading [`std::env::args`] itself, so
    /// the whole argument surface is testable without a process.
    ///
    /// # Errors
    /// [`Usage`] for any argument other than `--print-config`. Unknown flags
    /// are refused rather than ignored: a dog that silently ignored a flag
    /// it did not recognize would run with settings the caller thought they
    /// had changed, and the same logic applies to anything this binary
    /// might grow.
    ///
    /// A repeated `--print-config` is accepted. It names the same action
    /// however many times it is given, and refusing it meant answering
    /// `--print-config --print-config` with "shep-discord does not
    /// understand --print-config", which is a confusing thing to tell
    /// somebody who plainly does.
    ///
    /// `--version` and `--schema` are refused here and answered elsewhere,
    /// which is not a contradiction. `probe` reads the first argument,
    /// because the first argument is the only one shep passes a candidate
    /// binary, so a probe flag anywhere else reaches this parser instead.
    /// Refusing it with the general message would tell somebody that this
    /// binary does not understand a flag it plainly does, so it gets its own.
    pub fn parse<'a, I: IntoIterator<Item = &'a str>>(args: I) -> Result<Self, Usage> {
        let mut action = Self::Run;
        for arg in args {
            match arg {
                "--print-config" => action = Self::PrintConfig,
                "--help" | "-h" => {
                    return Err(Usage("shep-discord takes no options.".to_owned()));
                }
                probe @ ("--version" | "--schema") => {
                    return Err(Usage(format!(
                        "shep-discord answers {probe} as its first argument only, which is \
                         where the shepherd asks it."
                    )));
                }
                other => {
                    return Err(Usage(format!("shep-discord does not understand {other}.")));
                }
            }
        }
        Ok(action)
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
struct Identity {
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
    fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
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

/// How often the run loop rechecks [`Live::link`] and rereads its own
/// `dogs.toml` section.
///
/// Fixed rather than read from `[discord]`, because nothing in that section
/// governs this yet. Once the Discord client and the shepherd's own event
/// bus are wired into this loop it stops polling on a timer at all,
/// waiting on those instead; thirty seconds is short enough that a
/// handshake refused after the fact is noticed promptly and long enough
/// not to ask the shepherd for a section nothing yet acts on many times a
/// second.
const RECHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Whether a wait ended in a stop request rather than the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interrupted {
    /// The interval elapsed.
    No,
    /// A stop was requested first, or had been already.
    Yes,
}

/// Sleep for `interval`, or until a stop is requested.
async fn wait(interval: Duration, stop: &mut Stop) -> Interrupted {
    tokio::select! {
        // Biased, stop first: a stop already requested wins over a sleep
        // that is also ready, rather than the coin toss an unbiased select
        // would make of it.
        biased;
        () = stop.wait() => Interrupted::Yes,
        () = tokio::time::sleep(interval) => Interrupted::No,
    }
}

/// The run loop.
///
/// Nothing in here is fatal except a signal and a refused handshake. A
/// failed cycle is printed and retried on the next interval, because the
/// shepherd restarting underneath a dog is ordinary rather than
/// exceptional, and exiting would ask the supervisor to restart this
/// process for a condition that resolves itself on its own.
///
/// There is no signal handling beyond `ctrl_c`, which is the clean-exit
/// path for an operator running this binary in a terminal; see
/// [`stop`] for why the shepherd owns everything else.
async fn run(socket: &Path, identity: &Identity) -> ExitCode {
    let mut stop = Stop::on_ctrl_c();
    let mut session: Option<Live> = None;

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
        if session.is_none() {
            match connect(socket, identity).await {
                Ok(live) => session = Some(live),
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
                // Streaming only when a channel names somewhere to send to:
                // an operator who never set `log_channel` or `err_channel`
                // gets no bus subscription spent on lines nothing reads.
                // This await does not return until that subscription ends
                // or `stop` resolves, so it stands in for this cycle's
                // ordinary work for as long as it runs; when it returns,
                // the loop's own wait and reconnect below try again.
                Ok(config) if config.log_channel.is_some() || config.err_channel.is_some() => {
                    let handshake = identity.handshake.as_deref();
                    if let Err(err) =
                        session::stream_once(live, handshake, &config, &mut stop).await
                    {
                        eprintln!("shep-discord: {err}");
                    }
                }
                Ok(_config) => {}
                Err(err) => eprintln!("shep-discord: {err}"),
            }
        }

        if wait(RECHECK_INTERVAL, &mut stop).await == Interrupted::Yes {
            return ExitCode::SUCCESS;
        }
    }
}

fn main() -> ExitCode {
    // First, before this process opens a socket or a file. `shep adopt`
    // spawns this binary with `--version` and then with `--schema`, reads
    // one line of stdout, and kills the process group; a dog that has
    // already started connecting is a dog answering a question late.
    // `probe` answers either flag and exits, and returns for every other
    // run, this dog's own `--print-config` included.
    //
    // `env!` here rather than inside `probe`: it expands where it is
    // written, so a call that spelled it in shep-client would report
    // shep-client's version as this dog's.
    shep_client::dogs::probe::<config::Section>(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = match Action::parse(args.iter().map(String::as_str)) {
        Ok(action) => action,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    };

    if action == Action::PrintConfig {
        println!("{}", config::PRINT_CONFIG);
        return ExitCode::SUCCESS;
    }

    let env = |key: &str| std::env::var(key).ok();
    // The same reading shep's own CLI takes: `$SHEP_HOME` decides on its
    // own when it is set, and `$HOME` is only needed for the default it
    // replaces. An adopted dog always has `$SHEP_HOME`, so the third arm is
    // for somebody running this binary by hand in a stripped environment.
    let home_dir = match (std::env::var_os("HOME"), env("SHEP_HOME")) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(_)) => PathBuf::new(),
        (None, None) => {
            eprintln!("{NO_SHEP_HOME_MESSAGE}");
            return ExitCode::FAILURE;
        }
    };
    let paths = ShepPaths::resolve(&env, &home_dir);
    // Settled here, once `env` exists, rather than wherever first needs it:
    // it comes out of the environment shep spawned this process with, and
    // that answer cannot change while this process runs.
    let identity = Identity::from_env(env);

    // Built by hand rather than through `#[tokio::main]`: a runtime this
    // crate drops still waits for every task it spawned, and shutting it
    // down in the background instead means an exit does not sit through
    // work already underway on ctrl-c.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("{}", cannot_start_runtime_message(&err));
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(run(&paths.socket, &identity));
    runtime.shutdown_background();
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_no_dashes;

    /// No em dashes and no en dashes in anything this binary prints for a
    /// person: a terminal that cannot render one prints a replacement
    /// character in the middle of the one message that exists to be read by
    /// somebody who is already confused.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        assert_no_dashes(USAGE);
        let usage = Action::parse(["--bogus"]).expect_err("refused").to_string();
        assert_no_dashes(&usage);
        assert_no_dashes(config::PRINT_CONFIG);
        assert_no_dashes(&unadopted_message(DEFAULT_NAME));
        assert_no_dashes(&refused_message(Some("9"), "protocol too old"));
        assert_no_dashes(NO_SHEP_HOME_MESSAGE);
        assert_no_dashes(&cannot_start_runtime_message(&std::io::Error::other(
            "no more file descriptors",
        )));
    }

    /// An environment holding exactly one variable, which is the only one
    /// [`Identity::from_env`] reads.
    fn only(key: &str, value: &str) -> impl Fn(&str) -> Option<String> {
        let (key, value) = (key.to_owned(), value.to_owned());
        move |asked| (asked == key).then(|| value.clone())
    }

    #[test]
    fn print_config_is_the_only_argument() {
        assert_eq!(Action::parse(["--print-config"]), Ok(Action::PrintConfig));
        assert_eq!(Action::parse([]), Ok(Action::Run));
        assert!(Action::parse(["--token"]).is_err());
    }

    #[test]
    fn a_probe_flag_out_of_first_position_is_told_where_it_belongs() {
        for flag in ["--version", "--schema"] {
            let usage = Action::parse(["--print-config", flag])
                .expect_err("refused")
                .to_string();
            assert!(usage.contains(flag), "{usage}");
            assert!(usage.contains("first argument"), "{usage}");
            assert!(!usage.contains("does not understand"), "{usage}");
        }
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
}
