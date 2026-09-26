# shep-discord

A Discord dog for shep. Two tasks in one process: a serenity gateway
connection and a shep bus subscription, sharing one config section.

## Commands

- `cargo test --locked --bins --tests` is the test shape. There is no lib
  target, so `cargo test --lib` errors out with "no library targets found",
  and a bare `cargo test --locked` also runs `tests/probe.rs` as a separate
  test binary, which is included on purpose.
- The four lint gates CI runs, all required: `cargo fmt --all -- --check`,
  `cargo clippy --locked --all-targets --all-features -- -D warnings`,
  `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features`,
  and `cargo +1.88 check --all-targets --all-features --locked`.
- `cargo llvm-cov --locked --summary-only` for coverage. The unit tier sits
  above 92% of lines; `main.rs` and `bot/mod.rs` are the low files, because
  both need a live gateway or daemon connection to exercise fully.
- `tests/probe.rs` spawns the built binary and checks `--version` and
  `--schema` the way `shep adopt` does, with `SHEP_HOME` pointed at a
  temporary directory so it never touches a real shepherd.

## The 500-line file cap

500 lines is a question, not a hard limit: ask whether the file needs to be
that long before adding more to it. 1000 is a stop: split it, or say why
not. Measure PRODUCTION lines, not the whole file, since a long `#[cfg(test)]`
module at the bottom is not the thing the cap is about:

```sh
grep -n "^#\[cfg(test)\]" <file> | tail -1
```

`tail -1` on purpose. `grep ... | awk '... {exit}'` or any other
first-match form stops at the FIRST `#[cfg(test)]` it finds, which is wrong
the moment a file nests a second test module or has an inline `#[cfg(test)]`
helper above the real one; that shape produced a false compliance claim
during this crate's own build. There is exactly one real test module per
file here, at the bottom, so the last match is the one that counts.

## Rules the tests already enforce

- `Request::Flush` truncates the log files it names. It has exactly one
  legal construction site: `Verb::Flush` inside `Live::act` in
  `src/shepherd.rs`, reached only from `/shep flush`. Unlike shep-log-rotate,
  which forbids the request outright with a test scanning its own source,
  this crate cannot do that: `/shep flush` is a legitimate caller. A second
  caller anywhere else is a bug, not a feature.
- No em dash or en dash in anything a person reads: an error message, a
  Discord reply, a log line, a doc comment meant for a person rather than
  rustdoc syntax. `test_support::assert_no_dashes` and
  `assert_no_dashes_deep` are the checks; use one on any new user-facing
  string or JSON value.
- `Debug` on a type holding the bot token is written by hand and pinned by
  an exact-string test. `config::Section` and `config::Config` are the
  examples; both print `<redacted>` in place of `token` regardless of
  whether one is set.
- The `token` field of `config::Section` carries `x-shep-secret` in the
  generated schema, and no other property does. `dog_config` puts the mark
  on the field, so a build that compiles proves only that the attribute
  ran: a `Section` with the `#[shep(secret)]` line deleted still compiles
  and still passes every other test. `config.rs` reads the mark back out of
  `config_schema::<Section>()`, and `tests/probe.rs` reads it out of what
  the spawned binary prints for `--schema`.
- Every fallible `pub fn` has a `# Errors` section.
- `#![forbid(unsafe_code)]` at the crate root, in `src/main.rs`.

## Where things live

- `src/shepherd.rs` is the only module that builds a `Request` or reads a
  `Response`. Everything else that needs the shepherd goes through `Live`'s
  own methods instead of matching on the wire types itself, so a protocol
  change touches one file rather than every call site that happened to need
  a sheep's status.
- `src/session.rs` resolves `own_id`, the id this dog filters its own
  output by so it never republishes its own log lines into the channel it
  writes to. Three cases, not two: unadopted (nothing to filter), a
  resolved numeric id (filter it), and a dog that announced itself in the
  handshake but has not yet seen its own registration in a muster-roll
  refresh (still filtered, by name rather than id yet). Collapsing the
  third case into either of the other two is wrong in a different way each
  time.
- `src/stream/pack.rs` chunks a group of log lines to Discord's embed
  limits and packs chunks onto messages under Discord's real per-message
  budget: 6,000 characters summed across every embed field on one message,
  not a count of embeds and not a per-embed cap. This has been undercounted
  five times in this crate's history; read the module doc before touching
  it.
- `main` calls `shep_client::dogs::probe::<config::Section>` on its first
  line, before the argument parser. That answers the two questions `shep
  adopt` spawns this binary to ask: `--version` for the build and the
  protocol number, `--schema` for a JSON Schema of the `[discord]` section.
  `Action::parse` refuses every flag it does not know, so a probe flag that
  reached it would exit 1 with a usage message, which shep reads as no
  answer at all. `tests/probe.rs` spawns the binary to pin that order.
- Config lives in `[<name>]` of `dogs.toml`, keyed by `$SHEP_DOG_NAME`
  (`discord` when unset). `PRINT_CONFIG` in `src/config.rs`, the README and
  every error message have to agree on that name and on every setting's
  default; `config.rs`'s own round-trip test is the check.
- `shep-client` comes by version from crates.io and is the only path to
  shep-core: `shep_client::shep_core`, never a second direct dependency. The
  floor is 0.10.0, because the `DogConfig` derive is gone from it. The
  `dog_config` attribute replaced it, and `#[dog_config]` goes ABOVE the
  `#[derive(...)]` line on `config::Section`. rustc expands the attributes
  above an attribute macro before running it, so a `JsonSchema` derive
  listed higher has already built its impl by the time the mark goes on the
  field and the mark reaches no schema. Today that order is a compile error
  naming the fix rather than a config pane that quietly stops masking a
  token, but only because `Section` has a field to mark: the guard is
  `!marked.is_empty() && !derives_json_schema`, so a config type carrying
  no `#[shep(secret)]` takes the wrong order in silence. Read the order
  rather than trusting rustc to read it for you. The older floor still
  holds underneath: `Request::HostUsage` (what `/system` answers from)
  arrived in 0.8.2 and is absent from 0.7.4 and from 0.8.0. That raises
  the protocol this dog announces from 8 to 9, which does not lock out an
  older shepherd: a shepherd accepts any peer at
  or above its own `MIN_SUPPORTED`, still 8. What it does mean is a
  shepherd too old to know `HostUsage` answers `/system` with an error
  instead of an embed.

## Style

- Invoke the `rust-house-style` skill before writing or reviewing Rust. The
  rules are shep-pm/rust-house-style, IR-1..IR-48.
- Doc comments here are long on purpose and explain the decision, not the
  syntax. Match that for new items rather than trimming to a one-liner.
- `.coderabbit.yaml` restates the Rust rules reviewers hold this crate to.
  Read its `path_instructions` before touching `src/stream/pack.rs` or
  `src/session.rs`.
- Terminology: a `sheep` is one managed process, the plural is `flock`,
  dogs are plugin processes, the daemon is only ever "the shepherd".
