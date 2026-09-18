//! The one flag this binary accepts, and the message it prints when handed
//! anything else.
//!
//! Kept apart from [`crate::main`] and its run loop because this is a
//! different concern with a different reason to change: a new flag, or a
//! change to what an existing one means, touches only what is in here.
//! shep's own two probe flags, `--version` and `--schema`, are not part of
//! this surface at all; they are answered and exited on by
//! [`shep_client::dogs::probe`] before [`crate::main`] ever calls
//! [`Action::parse`], so this parser only ever sees a run that is not a
//! probe, and refuses those two flags out of any other position with a
//! message that says where they belong instead of the general
//! "does not understand" one.

use core::fmt;

/// Everything this binary accepts, printed when it is handed anything else.
///
/// No em dashes and no en dashes: a terminal that cannot render one prints a
/// replacement character in the middle of the one message that exists to be
/// read by somebody who is already confused.
pub const USAGE: &str = "\
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

/// What this process was asked to do.
///
/// One flag, so no `clap`: a dependency that parses one argument would be
/// larger than the whole of this binary's argument surface, and that surface
/// is deliberately closed. Everything configurable is configured in
/// `dogs.toml`, where the shepherd can serve it.
///
/// shep's own two flags are not in here. `--version` and `--schema` are
/// answered and exited on by [`shep_client::dogs::probe`] before
/// [`crate::main`] builds an [`Action`] at all, so this enum only ever sees a
/// run that is not a probe.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_no_dashes;

    /// No em dashes and no en dashes in anything this parser prints for a
    /// person: a terminal that cannot render one prints a replacement
    /// character in the middle of the one message that exists to be read by
    /// somebody who is already confused.
    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        assert_no_dashes(USAGE);
        let usage = Action::parse(["--bogus"]).expect_err("refused").to_string();
        assert_no_dashes(&usage);
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
}
