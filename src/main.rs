//! `shep-discord`: a Discord dog for shep.
//!
//! Streams a sheep's stdout and stderr into Discord channels, answers
//! shep's own verbs from slash commands, and keeps one live embed per sheep
//! in a monitor channel.
//!
//! This file is about STARTING a process: the probe shep spawns the binary
//! to ask, the arguments it was given, where its shep home is, and the
//! runtime everything else runs on. Being one is [`run`]'s job, and parsing
//! the arguments is [`cli`]'s; each is a separate concern with its own
//! reason to change.
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
//! own type. [`run::Identity`] holds both and its module doc has the
//! argument; `main`'s part is reading them out of the environment once,
//! here, where the environment is read at all.

#![forbid(unsafe_code)]

mod bot;
mod cli;
mod config;
mod error;
mod limits;
mod names;
mod run;
mod session;
mod shepherd;
mod stop;
mod stream;
#[cfg(test)]
mod test_support;
mod wiring;

use std::{path::PathBuf, process::ExitCode};

use shep_client::shep_core::paths::ShepPaths;

use crate::{cli::Action, run::Identity};

/// The message printed when neither `$HOME` nor `$SHEP_HOME` is set.
///
/// A `const` rather than an inline `eprintln!`: it lets the dash check
/// reach the text without running [`main`], which would call `probe` and
/// read the real process environment. [`crate::run`]'s own messages are
/// functions for the same reason.
const NO_SHEP_HOME_MESSAGE: &str = "shep-discord: neither $HOME nor $SHEP_HOME is set, so there \
                                     is no shep home to find a socket in.";

/// The message printed when the Tokio runtime fails to build.
///
/// A function rather than an inline `eprintln!`, the same reason
/// [`NO_SHEP_HOME_MESSAGE`] is a `const`.
fn cannot_start_runtime_message(err: &std::io::Error) -> String {
    format!("shep-discord: cannot start a runtime: {err}")
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
    let code = runtime.block_on(run::run(&paths.socket, &identity));
    runtime.shutdown_background();
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_no_dashes;

    /// No em dashes and no en dashes in anything this file prints for a
    /// person: a terminal that cannot render one prints a replacement
    /// character in the middle of the one message that exists to be read by
    /// somebody who is already confused. [`cli`]'s own dash test covers
    /// [`cli::USAGE`] and the messages [`Action::parse`] builds, and
    /// [`run`]'s covers the ones its loop prints.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        assert_no_dashes(config::PRINT_CONFIG);
        assert_no_dashes(NO_SHEP_HOME_MESSAGE);
        assert_no_dashes(&cannot_start_runtime_message(&std::io::Error::other(
            "no more file descriptors",
        )));
    }
}
