# shep-discord Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `shep-discord`, a shep dog that streams sheep log output into Discord channels and exposes shep's verbs as Discord slash commands with a live monitor.

**Architecture:** One binary. A `ReconnectingClient` to the shepherd's Unix socket carries both a bus subscription (for log lines and process events) and ordinary requests. A serenity gateway client runs alongside it. `shepherd.rs` is the only module that builds a `Request`, so every other module is testable without a socket. Config arrives over the socket from `[discord]` in `dogs.toml`, never from the environment.

**Tech Stack:** Rust 2024, `shep-client` 0.8.2, `serenity` 0.12, `tokio`, `schemars`, `serde`, `toml`.

**Spec:** [`docs/brainstorming/specs/2026-09-18-shep-discord-design.md`](../../brainstorming/specs/2026-09-18-shep-discord-design.md)

## Global Constraints

Every task's requirements implicitly include this section.

- `#![forbid(unsafe_code)]` at the crate root.
- Edition 2024, `rust-version = "1.88"`.
- Licence `MIT OR Apache-2.0`.
- `shep-client = "0.8.2"` is the only path to shep-core. Reach it as `shep_client::shep_core`, never a second direct dependency. The floor is 0.8.2 because `Request::HostUsage`, which `/system` is built on, is absent from 0.7.4 and 0.8.0.
- Every fallible `pub fn` carries a `# Errors` section. Errors implement `core::error::Error`, never `std::error::Error`.
- No em dash and no en dash in any string printed for a person. `test_support::assert_no_dashes` is the check.
- A type holding a secret or a socket path gets a hand-written `Debug` that redacts it, pinned by an exact-string test.
- `Request::Flush` may be constructed only in the `/shep flush` path. No other module may name it.
- Keep every `.rs` file under 500 lines. `~/.claude/hooks/file-size-guard.js` asks at 500 and refuses at 1000.
- The four lint gates, all required: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features`, `cargo +1.88 check --all-targets --all-features --locked`.
- Discord limits, from the spec: `description` 4,096 characters; `title` 256; the combined sum across `title`, `description`, `field.name`, `field.value`, `footer.text` and `author.name` over every embed on one message must not exceed 6,000; `custom_id` 1 to 100 characters; at most 5 buttons in an action row.
- Doc comments explain the decision, not the syntax. Match shep-log-rotate's density, which is high on purpose.

---

### Task 1: Crate scaffolding, config type, and the probe

The binary has to answer `shep adopt`'s two questions before it does anything else, and answering `--schema` needs the real config type. Those ship together.

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `LICENSE-MIT`, `LICENSE-APACHE`
- Create: `src/main.rs`, `src/config.rs`, `src/error.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `config::Section` (the `Deserialize` + `JsonSchema` + `DogConfig` type), `config::Config` (the resolved settings with a default for every optional field), `error::Error`, `main::Action`, `main::Usage`.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "shep-discord"
version = "0.1.0"
edition = "2024"
rust-version = "1.88"
description = "A Discord dog for shep: streams sheep logs to a channel and drives the flock from slash commands"
repository = "https://github.com/shep-pm/shep-discord"
license = "MIT OR Apache-2.0"
readme = "README.md"
keywords = ["shep", "process-manager", "discord", "bot"]
categories = ["command-line-utilities"]

[dependencies]
# The only path to shep-core: `shep_client::shep_core`, never a second
# direct dependency. 0.8.2 is the floor because `Request::HostUsage` is,
# and `/system` is built on it. It is absent from 0.7.4 and from 0.8.0.
# The two older reasons still hold underneath: the `schema` feature
# forwards down to shep-core so `MemSize` and `UpDuration` carry their
# `JsonSchema` impls, and the protocol number a dog announces has to be at
# or above the shepherd's `MIN_SUPPORTED`. That number is still 8, so
# announcing 9 locks out nothing.
shep-client = "0.8.2"
# `#[derive(JsonSchema)]` expands to absolute `schemars::` paths, so the
# crate has to be nameable here even though shep-client carries it.
schemars = { version = "1.2.2", default-features = false, features = ["derive", "std"] }
serde = { version = "1", features = ["derive"] }
toml = "0.8"
tokio = { version = "1", default-features = false, features = ["rt-multi-thread", "macros", "time", "signal", "sync"] }
# No `cache`: this dog holds its own state and a second copy would drift.
# No `framework`/`standard_framework`: those serve prefix commands, which
# have nothing to do with slash commands.
serenity = { version = "0.12", default-features = false, features = ["client", "gateway", "model", "builder", "rustls_backend"] }

[dev-dependencies]
shep-client = { version = "0.8.2", features = ["test-support"] }
serde_json = "1"
tempfile = "3"

# The same profile settings shep and shep-log-rotate carry, for the same
# reasons. Debug info dominates a debug build and a dependency's is never
# what is being debugged.
[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
debug = false

# Size, not speed. Not `strip`, and not `panic = "abort"`: symbols are what
# a profiler names frames with, and `shep-client` re-raises a panicked task
# with `resume_unwind`, which needs a runtime that unwinds.
[profile.release]
lto = "thin"
codegen-units = 1
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

`.gitignore`:

```
/target
```

Copy `LICENSE-MIT` and `LICENSE-APACHE` verbatim from `shep-log-rotate`.

- [ ] **Step 2: Write the failing test for `Section`'s redacted `Debug`**

In `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A derived `Debug` on a type holding a bot token puts that token into
    /// every log line, panic message and error chain that prints it. An
    /// exact string, not a `contains`: a redaction that stops covering a
    /// newly added field still passes a `contains` check.
    #[test]
    fn the_token_never_reaches_a_debug_line() {
        let section = Section {
            token: Some("MTIzNDU2Nzg5.GaBcDe.ThisIsNotARealToken".to_owned()),
            guild_id: Some(42),
            ..Section::default()
        };
        let rendered = format!("{section:?}");
        assert_eq!(
            rendered,
            "Section { token: <redacted>, guild_id: Some(42), monitor_channel: None, \
             monitor_interval: None, log_channel: None, err_channel: None, flush: None, \
             coalesce: None, buffer_lines: None, ignore_dogs: None }"
        );
        assert!(!rendered.contains("ThisIsNotARealToken"), "{rendered}");
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test --locked the_token_never_reaches_a_debug_line`
Expected: FAIL, `cannot find struct `Section``.

- [ ] **Step 4: Write `src/config.rs`**

```rust
//! The `[discord]` section of `dogs.toml`.
//!
//! The daemon serves this per request rather than caching it, and it never
//! travels through the environment: an environment variable is readable
//! from the process table, inherited by every child, and captured into a
//! crash dump. A bot token is a bearer credential, so that matters here
//! more than it does for most dogs.

use core::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use shep_client::{dogs::DogConfig, shep_core::values::UpDuration};

/// What an operator may write under `[discord]`. Every field is optional so
/// a half-filled section still parses and the error names the missing key,
/// rather than serde refusing the whole table.
#[derive(Default, Deserialize, JsonSchema, DogConfig)]
#[serde(deny_unknown_fields)]
#[schemars(title = "discord", description = "Settings for the shep-discord dog.")]
pub struct Section {
    /// The bot token. Marked secret so lookout's config pane masks it and
    /// the published schema says what it is.
    #[shep(secret)]
    #[schemars(description = "Discord bot token.")]
    pub token: Option<String>,
    #[schemars(description = "The guild (server) this bot serves.")]
    pub guild_id: Option<u64>,
    #[schemars(description = "Channel for the live monitor. Unset disables it.")]
    pub monitor_channel: Option<u64>,
    // `with` names shep-core's own grammar while the field stays a
    // `String`, because an operator writes "1m" and lookout needs the
    // `$ref` name to tell a duration from a byte size. This is the shape
    // shep-log-rotate's `Section` uses for exactly this reason.
    #[schemars(with = "Option<UpDuration>", description = "How often the monitor refreshes, e.g. \"1m\". Unset means the monitor does not run from boot.")]
    pub monitor_interval: Option<String>,
    #[schemars(description = "Channel for stdout lines. Unset disables that stream.")]
    pub log_channel: Option<u64>,
    #[schemars(description = "Channel for stderr lines. Unset disables that stream.")]
    pub err_channel: Option<u64>,
    #[schemars(with = "Option<UpDuration>", description = "How often the buffer drains, e.g. \"1s\".")]
    pub flush: Option<String>,
    #[schemars(with = "Option<UpDuration>", description = "How wide a window joins lines into one embed, e.g. \"1s\".")]
    pub coalesce: Option<String>,
    #[schemars(description = "How many lines the buffer holds before dropping oldest.")]
    pub buffer_lines: Option<usize>,
    #[schemars(description = "Hide other dogs from listings and the monitor.")]
    pub ignore_dogs: Option<bool>,
}

impl fmt::Debug for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Section")
            .field("token", &"<redacted>")
            .field("guild_id", &self.guild_id)
            .field("monitor_channel", &self.monitor_channel)
            .field("monitor_interval", &self.monitor_interval)
            .field("log_channel", &self.log_channel)
            .field("err_channel", &self.err_channel)
            .field("flush", &self.flush)
            .field("coalesce", &self.coalesce)
            .field("buffer_lines", &self.buffer_lines)
            .field("ignore_dogs", &self.ignore_dogs)
            .finish()
    }
}

/// The resolved settings, with every optional field decided.
pub struct Config {
    pub token: String,
    pub guild_id: u64,
    pub monitor_channel: Option<u64>,
    pub monitor_interval: Option<UpDuration>,
    pub log_channel: Option<u64>,
    pub err_channel: Option<u64>,
    pub flush: UpDuration,
    pub coalesce: UpDuration,
    pub buffer_lines: usize,
    pub ignore_dogs: bool,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("token", &"<redacted>")
            .field("guild_id", &self.guild_id)
            .field("monitor_channel", &self.monitor_channel)
            .field("monitor_interval", &self.monitor_interval)
            .field("log_channel", &self.log_channel)
            .field("err_channel", &self.err_channel)
            .field("flush", &self.flush)
            .field("coalesce", &self.coalesce)
            .field("buffer_lines", &self.buffer_lines)
            .field("ignore_dogs", &self.ignore_dogs)
            .finish()
    }
}

/// Defaults, named once so `PRINT_CONFIG` and the parser cannot disagree.
pub const DEFAULT_FLUSH_MS: u64 = 1_000;
pub const DEFAULT_COALESCE_MS: u64 = 1_000;
pub const DEFAULT_BUFFER_LINES: usize = 2_000;
/// The floor `monitor_interval` is clamped up to, carried over from the old
/// 0.25 minute minimum. A monitor refreshing faster than this spends the
/// channel's whole rate budget redrawing embeds nobody asked for.
pub const MIN_MONITOR_INTERVAL_MS: u64 = 15_000;
```

- [ ] **Step 5: Run the test**

Run: `cargo test --locked the_token_never_reaches_a_debug_line`
Expected: PASS.

- [ ] **Step 6: Write `src/error.rs`**

```rust
//! Everything that can go wrong, in one enum.

use core::fmt;

use shep_client::{ConnectError, RequestError};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The first connection, or a reconnect the supervisor gave up on.
    Connect(ConnectError),
    /// A request the shepherd refused or could not answer.
    Request(RequestError),
    /// The shepherd answered, with something else. Names both sides,
    /// because "unexpected response" alone sends the reader to the wrong
    /// end of the wire.
    Unexpected { asked: String, got: String },
    /// `dogs.toml`'s `[discord]` section did not parse, or a value in it
    /// is outside what this dog accepts.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "cannot reach the shepherd: {err}"),
            Self::Request(err) => write!(f, "the shepherd refused a request: {err}"),
            Self::Unexpected { asked, got } => {
                write!(f, "asked the shepherd for {asked} and got {got}")
            }
            Self::Config(message) => write!(f, "[discord] in dogs.toml: {message}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Request(err) => Some(err),
            Self::Unexpected { .. } | Self::Config(_) => None,
        }
    }
}

impl From<ConnectError> for Error {
    fn from(err: ConnectError) -> Self {
        Self::Connect(err)
    }
}

impl From<RequestError> for Error {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}
```

- [ ] **Step 7: Write `src/main.rs` with the probe and the argument parser**

Port `Action`, `Usage` and `Identity` from `shep-log-rotate/src/main.rs`, changing only the binary name in the strings and `DEFAULT_NAME` to `"discord"`. The rules that must survive the copy verbatim, because each one is there for a failure that already happened:

- `shep_client::dogs::probe::<config::Section>(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))` is `main`'s first line, before the argument parser. `shep adopt` spawns the binary with `--version` and then `--schema`, reads one line of stdout, and kills the process group.
- `Action::parse` refuses every flag it does not know. A rotator that silently ignored `--dry-run` would rotate for real; the same logic applies to anything this binary might grow.
- `--version` and `--schema` reaching `Action::parse` get their own refusal naming first-argument position, not the general "does not understand" message, because this binary plainly does understand them.
- `Identity` keeps two names in two fields. The handshake name comes from `$SHEP_DOG_NAME` and is never guessed, because a refused handshake is recorded against whatever the frame said, so an invented name asks the daemon to restart somebody else's dog. The config section falls back to `DEFAULT_NAME`.

- [ ] **Step 8: Write the argument tests**

```rust
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
```

- [ ] **Step 9: Verify the probe answers**

```bash
cargo build --locked
./target/debug/shep-discord --version
./target/debug/shep-discord --schema | python3 -m json.tool | grep -A2 '"token"'
```

Expected: the version line carries `shep-protocol:`; the schema shows `"x-shep-secret": true` on `token`.

- [ ] **Step 10: Run every gate and commit**

```bash
cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --locked
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore LICENSE-MIT LICENSE-APACHE src/
git commit -m "feat: scaffold the crate and answer shep's adopt probes"
```

---

### Task 2: Config parsing, bounds, and PRINT_CONFIG

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Consumes: `config::Section`, `config::Config`, `error::Error` from Task 1.
- Produces: `Config::from_toml(&str) -> Result<Config, Error>`, `config::PRINT_CONFIG: &str`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_section_without_a_token_names_the_missing_key() {
    let err = Config::from_toml("guild_id = 1").expect_err("refused").to_string();
    assert!(err.contains("token"), "{err}");
}

#[test]
fn an_empty_section_still_names_what_is_missing() {
    // The daemon answers `DogConfig` for a name nobody adopted with an
    // empty section, byte for byte what a dog on its defaults gets. So
    // this is the likeliest error an operator sees, and it has to be the
    // clearest one.
    let err = Config::from_toml("").expect_err("refused").to_string();
    assert!(err.contains("token"), "{err}");
}

#[test]
fn a_monitor_interval_under_the_floor_is_raised_to_it() {
    let config = Config::from_toml(
        "token = \"t\"\nguild_id = 1\nmonitor_interval = \"1s\"\n",
    )
    .expect("parsed");
    assert_eq!(
        config.monitor_interval.expect("set").as_millis(),
        MIN_MONITOR_INTERVAL_MS,
        "a monitor refreshing every second spends the channel's whole rate budget"
    );
}

#[test]
fn a_misspelled_key_is_refused_rather_than_ignored() {
    let err = Config::from_toml("token = \"t\"\nguild_id = 1\nbuffer_line = 10\n")
        .expect_err("refused")
        .to_string();
    assert!(err.contains("buffer_line"), "{err}");
}

#[test]
fn the_defaults_are_what_the_printed_block_says() {
    let config = Config::from_toml("token = \"t\"\nguild_id = 1\n").expect("parsed");
    assert_eq!(config.flush.as_millis(), DEFAULT_FLUSH_MS);
    assert_eq!(config.coalesce.as_millis(), DEFAULT_COALESCE_MS);
    assert_eq!(config.buffer_lines, DEFAULT_BUFFER_LINES);
    assert!(!config.ignore_dogs);
}

/// The schema and the printed block are the second and third places that
/// have to agree with `Section`'s fields about what a setting is. This is
/// the edge between them.
#[test]
fn the_schema_and_the_printed_block_name_the_same_settings() {
    let schema = shep_client::dogs::config_schema::<Section>().expect("schema");
    let value = serde_json::to_value(&schema).expect("json");
    let properties = value["properties"].as_object().expect("properties");
    for key in properties.keys() {
        assert!(
            PRINT_CONFIG.contains(&format!("{key} =")) || PRINT_CONFIG.contains(&format!("# {key} =")),
            "PRINT_CONFIG omits {key}"
        );
    }
}

#[test]
fn nothing_printed_for_a_person_carries_a_dash() {
    crate::test_support::assert_no_dashes(PRINT_CONFIG);
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --locked config::tests`
Expected: FAIL, `no function or associated item named `from_toml``.

- [ ] **Step 3: Implement `from_toml` and `PRINT_CONFIG`**

`from_toml` parses the text into `Section` with `toml::from_str`, mapping the error through `Error::Config`; then resolves each field, returning `Error::Config` naming `token` or `guild_id` when either is absent; parses `flush`, `coalesce` and `monitor_interval` with `UpDuration`'s own `FromStr`, mapping a parse failure to `Error::Config` naming the key; and clamps `monitor_interval` up to `MIN_MONITOR_INTERVAL_MS`.

`PRINT_CONFIG` is a raw string starting `[discord]`, with every key commented at its default and a sentence per key. It names `dogs.toml` rather than `shep.toml`: writing the old header back after a shepherd has migrated a section names one dog in two files, which the daemon refuses to boot on.

- [ ] **Step 4: Run the tests**

Run: `cargo test --locked config::tests`
Expected: PASS, seven tests.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs && git commit -m "feat: parse the [discord] section and print a commented block"
```

---

### Task 3: stop.rs and test_support.rs

**Files:**
- Create: `src/stop.rs`, `src/test_support.rs`
- Modify: `src/main.rs` (declare the modules, wire `Stop` into the run loop)

**Interfaces:**
- Produces: `stop::Stop::on_ctrl_c() -> Stop`, `Stop::wait(&mut self) -> impl Future<Output = ()>`, `test_support::assert_no_dashes(&str)`.

- [ ] **Step 1: Copy both files from shep-log-rotate**

`src/stop.rs` (131 lines) and `src/test_support.rs` (33 lines) are copied unchanged. They are correct, they are tested, and rewriting them would be inventing a second ctrl-c handler with the same job.

Keep the module doc's reasoning: the shepherd owns this process's signals and its kill ladder, and ctrl-c is the clean-exit path for an operator running the binary in a terminal.

- [ ] **Step 2: Run their tests**

Run: `cargo test --locked stop::`
Expected: PASS, whatever count shep-log-rotate's `stop.rs` carries.

- [ ] **Step 3: Apply `assert_no_dashes` to the strings Task 1 wrote**

```rust
#[test]
fn the_usage_text_carries_no_em_dash() {
    let usage = Action::parse(["--nonsense"]).expect_err("refused").to_string();
    crate::test_support::assert_no_dashes(&usage);
}
```

- [ ] **Step 4: Run it, then commit**

```bash
cargo test --locked
git add src/stop.rs src/test_support.rs src/main.rs
git commit -m "feat: carry shep-log-rotate's stop handling and dash check"
```

---

### Task 4: shepherd.rs, the only module that builds a Request

**Files:**
- Create: `src/shepherd.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `error::Error`.
- Produces:
  - `shepherd::Live::new(ReconnectingClient) -> Live`
  - `Live::link(&self) -> LinkState`
  - `Live::section(&self, name: &str) -> Result<String, Error>`
  - `Live::flock(&self) -> Result<Vec<ProcessInfo>, Error>`
  - `Live::describe(&self, name: &str) -> Result<Option<ProcessInfo>, Error>`
  - `Live::host_usage(&self) -> Result<Option<HostUsage>, Error>`
  - `Live::act(&self, verb: Verb, selector: SelectorSpec) -> Result<String, Error>`
  - `Live::subscribe(&self, topics: Vec<String>) -> Result<EventStream, Error>`
  - `shepherd::Verb` (`Start`, `Stop`, `Restart`, `Reload`, `Delete`, `Flush`, `Save`, `Reopen`)

- [ ] **Step 1: Write the failing tests against a fake shepherd**

`shep-client`'s `test-support` feature binds a fake shepherd this crate can connect to. Write one test per verb asserting the `Request` that goes out and the sentence that comes back.

```rust
#[tokio::test]
async fn restart_names_the_sheep_in_its_reply() {
    let (live, mut fake) = test_live().await;
    fake.expect(Request::Restart { selector: SelectorSpec::Name("web".to_owned()) })
        .answer(Response::Restarted { accepted: vec![sample("web")], refused: Vec::new() });
    let reply = live.act(Verb::Restart, SelectorSpec::Name("web".to_owned())).await.expect("ok");
    assert_eq!(reply, "restarted web");
}

/// The shepherd answering something else is its own error, naming both
/// sides. "unexpected response" alone sends the reader to the wrong end of
/// the wire.
#[tokio::test]
async fn a_wrong_shaped_answer_names_what_was_asked_and_what_came_back() {
    let (live, mut fake) = test_live().await;
    fake.expect(Request::ListFlock).answer(Response::Pong);
    let err = live.flock().await.expect_err("refused").to_string();
    assert!(err.contains("a Flock"), "{err}");
    assert!(err.contains("Pong"), "{err}");
}

/// `Live` holds a socket path. A derived `Debug` would print it into any
/// error chain, and a path is one of the two things this crate redacts.
#[test]
fn a_live_session_never_prints_its_socket_path() {
    let live = Live::new(fake_client("/run/user/1000/shep/shep.sock"));
    assert_eq!(format!("{live:?}"), "Live { socket: <redacted> }");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --locked shepherd::`
Expected: FAIL, `cannot find type `Live``.

- [ ] **Step 3: Implement `shepherd.rs`**

The verb-to-request table, which is where PM2's vocabulary becomes shep's:

| `Verb` | `Request` | Success `Response` | Reply sentence |
|---|---|---|---|
| `Start` | `Start { apps }` via `Describe` first, or `Restart` on an existing sheep | `Started` | `started <name>` |
| `Stop` | `Stop { selector }` | `Stopped` | `stopped <name>` |
| `Restart` | `Restart { selector }` | `Restarted` | `restarted <name>` |
| `Reload` | `Reload { selector }` | `Reloading` | `reloading <name>` |
| `Delete` | `Delete { selector }` | `Deleted` | `deleted <name>` |
| `Flush` | `Flush { selector }` | `Flushed` | `flushed <name>` |
| `Save` | `SaveRoll` | `RollSaved { path, apps }` | `saved <apps> apps to <path>` |
| `Reopen` | `Reopen { selector }` | `Reopened` | `reopened <name>` |

`Restarted` and `Reloading` both carry `accepted` and `refused`. A reply naming only the accepted set hides a refusal, so the sentence names both when `refused` is non-empty.

`Live`'s `Debug` is hand-written and prints `Live { socket: <redacted> }`.

Only this module may construct `Request::Flush`, and only `Verb::Flush` reaches it.

- [ ] **Step 4: Run the tests**

Run: `cargo test --locked shepherd::`
Expected: PASS.

- [ ] **Step 5: Wire `connect` and the run loop into main.rs**

Copy shep-log-rotate's `connect`, `refused` and the loop posture: nothing is fatal except a signal and a refused handshake, a `LinkState::Refused` is checked before the work rather than after a failed attempt, and a failed cycle is printed and retried.

- [ ] **Step 6: Commit**

```bash
git add src/shepherd.rs src/main.rs
git commit -m "feat: talk to the shepherd through one module"
```

---

### Task 5: names.rs, the id to name cache

`BusEvent::LogOut` carries `{ id, line }` and no name. PM2's bus named the process on every frame, so neither source repo needed this.

**Files:**
- Create: `src/names.rs`

**Interfaces:**
- Consumes: `shepherd::Live`.
- Produces: `names::Names::new() -> Names`, `Names::refresh(&mut self, &[ProcessInfo])`, `Names::get(&self, id: u32) -> String`, `Names::id_of(&self, name: &str) -> Option<u32>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_unknown_id_is_named_rather_than_dropped() {
    let names = Names::new();
    assert_eq!(
        names.get(7),
        "sheep 7",
        "a line from a sheep the cache has not seen is still a line worth showing"
    );
}

#[test]
fn a_refresh_replaces_rather_than_merges() {
    // A sheep deleted between two refreshes must leave the cache, or its
    // name outlives it and a reused id shows the wrong one.
    let mut names = Names::new();
    names.refresh(&[info(1, "web"), info(2, "api")]);
    names.refresh(&[info(1, "web")]);
    assert_eq!(names.get(1), "web");
    assert_eq!(names.get(2), "sheep 2");
}

#[test]
fn a_name_resolves_back_to_its_id() {
    let mut names = Names::new();
    names.refresh(&[info(3, "worker")]);
    assert_eq!(names.id_of("worker"), Some(3));
    assert_eq!(names.id_of("ghost"), None);
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked names::`
Expected: FAIL, `cannot find type `Names``.

- [ ] **Step 3: Implement**

A `HashMap<u32, String>` behind a struct, replaced wholesale on refresh. `id_of` walks the map; the flock is small enough that a reverse index would be two things to keep in step for no measurable gain.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked names::
git add src/names.rs && git commit -m "feat: name a sheep from the id a log frame carries"
```

---

### Task 6: stream/buffer.rs, bounded buffering and coalescing

**Files:**
- Create: `src/stream/mod.rs` (module declarations only for now), `src/stream/buffer.rs`

**Interfaces:**
- Produces: `buffer::Line { at_ms: u64, name: String, text: String }`, `buffer::Buffer::new(capacity: usize)`, `Buffer::push(&mut self, Line)`, `Buffer::drain(&mut self, coalesce_ms: u64) -> Vec<Group>`, `Buffer::take_dropped(&mut self) -> u64`, `buffer::Group { name: String, at_ms: u64, text: String }`.

- [ ] **Step 1: Write the failing tests**

```rust
fn line(at_ms: u64, name: &str, text: &str) -> Line {
    Line { at_ms, name: name.to_owned(), text: text.to_owned() }
}

#[test]
fn lines_inside_the_window_join_into_one_group() {
    let mut buffer = Buffer::new(100);
    buffer.push(line(1_000, "web", "first"));
    buffer.push(line(1_400, "web", "second"));
    let groups = buffer.drain(1_000);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].text, "first\nsecond");
}

#[test]
fn a_line_past_the_window_starts_a_new_group() {
    let mut buffer = Buffer::new(100);
    buffer.push(line(1_000, "web", "first"));
    buffer.push(line(2_000, "web", "second"));
    let groups = buffer.drain(1_000);
    assert_eq!(groups.len(), 2, "the window is half open: [at, at + coalesce)");
}

/// The old code handled exactly one group per flush at `Discord.ts:87`, so
/// a backlog never cleared: every tick moved one group and left the rest.
#[test]
fn drain_returns_every_group_not_one() {
    let mut buffer = Buffer::new(100);
    for i in 0..5 {
        buffer.push(line(i * 5_000, "web", "line"));
    }
    assert_eq!(buffer.drain(1_000).len(), 5);
    assert!(buffer.drain(1_000).is_empty(), "drain leaves the buffer empty");
}

#[test]
fn two_sheep_in_one_window_do_not_share_a_group() {
    let mut buffer = Buffer::new(100);
    buffer.push(line(1_000, "web", "from web"));
    buffer.push(line(1_100, "api", "from api"));
    let groups = buffer.drain(1_000);
    assert_eq!(groups.len(), 2, "a group carries one sheep's name, so it holds one sheep's lines");
}

/// The old buffer grew without limit. A sheep in a crash loop printing a
/// stack trace per millisecond is the case that matters.
#[test]
fn over_capacity_the_oldest_lines_go_and_the_count_is_kept() {
    let mut buffer = Buffer::new(3);
    for i in 0..5 {
        buffer.push(line(i * 5_000, "web", "line"));
    }
    assert_eq!(buffer.take_dropped(), 2);
    assert_eq!(buffer.take_dropped(), 0, "taking the count clears it");
    assert_eq!(buffer.drain(1_000).len(), 3);
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked stream::buffer`
Expected: FAIL, `cannot find type `Buffer``.

- [ ] **Step 3: Implement**

A `VecDeque<Line>` plus a `dropped: u64`. `push` pops the front when at capacity and increments `dropped`. `drain` pops the front line, then keeps popping while the next line shares its `name` and its `at_ms` falls inside `[at, at + coalesce_ms)`, joining texts with `\n`; repeats until empty.

Grouping keys on the name as well as the window, because a group renders under one sheep's title.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked stream::buffer
git add src/stream/ && git commit -m "feat: buffer log lines with a bounded queue and a real window"
```

---

### Task 7: stream/pack.rs, chunking and the 6,000 character budget

This is where the old code was wrong in the way that mattered.

**Files:**
- Create: `src/stream/pack.rs`

**Interfaces:**
- Consumes: `buffer::Group`.
- Produces: `pack::EMBED_DESCRIPTION_LIMIT: usize = 4096`, `pack::MESSAGE_CHARACTER_BUDGET: usize = 6000`, `pack::Chunk { title: String, description: String }`, `pack::chunks(&Group) -> Vec<Chunk>`, `pack::into_messages(Vec<Chunk>) -> Vec<Vec<Chunk>>`, `pack::strip_ansi(&str) -> String`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_short_group_is_one_untitled_chunk() {
    let chunks = chunks(&group("web", "hello"));
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].title, "web");
    assert_eq!(chunks[0].description, "hello");
}

#[test]
fn a_long_group_splits_at_the_description_limit_and_numbers_itself() {
    let chunks = chunks(&group("web", &"x".repeat(EMBED_DESCRIPTION_LIMIT + 1)));
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].title, "web (1/2)");
    assert_eq!(chunks[1].title, "web (2/2)");
    assert_eq!(chunks[0].description.chars().count(), EMBED_DESCRIPTION_LIMIT);
    assert_eq!(chunks[1].description.chars().count(), 1);
}

/// The whole point of this module. Two full chunks is 8,192 characters,
/// and Discord's limit is the 6,000 sum across every embed on ONE message,
/// not an embed count. The old code sent every pending embed in one call
/// and took a 400 for it, then retried the same batch forever because it
/// cleared the queue only on success.
#[test]
fn two_full_chunks_never_ride_on_one_message() {
    let chunks = chunks(&group("web", &"x".repeat(EMBED_DESCRIPTION_LIMIT * 2)));
    assert_eq!(chunks.len(), 2);
    let messages = into_messages(chunks);
    assert_eq!(messages.len(), 2, "each full chunk needs its own message");
    for message in &messages {
        let total: usize = message
            .iter()
            .map(|chunk| chunk.title.chars().count() + chunk.description.chars().count())
            .sum();
        assert!(total <= MESSAGE_CHARACTER_BUDGET, "{total} over budget");
    }
}

#[test]
fn small_chunks_share_a_message_until_the_budget_is_spent() {
    let chunks: Vec<Chunk> = (0..10)
        .map(|i| Chunk { title: format!("sheep{i}"), description: "x".repeat(1_000) })
        .collect();
    let messages = into_messages(chunks);
    assert!(messages.len() >= 2);
    for message in &messages {
        let total: usize = message
            .iter()
            .map(|chunk| chunk.title.chars().count() + chunk.description.chars().count())
            .sum();
        assert!(total <= MESSAGE_CHARACTER_BUDGET, "{total} over budget");
    }
    assert_eq!(messages.iter().map(Vec::len).sum::<usize>(), 10, "no chunk is lost");
}

#[test]
fn the_budget_counts_characters_not_bytes() {
    // Discord counts characters. A description of 4,096 multibyte
    // characters is legal and roughly 12 KiB on the wire, so a byte-based
    // split would refuse a message Discord accepts and, worse, a
    // byte-based limit check would let an over-long one through.
    let chunks = chunks(&group("web", &"é".repeat(EMBED_DESCRIPTION_LIMIT)));
    assert_eq!(chunks.len(), 1, "4,096 characters is one chunk however many bytes it is");
}

#[test]
fn ansi_escapes_do_not_reach_discord() {
    assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked stream::pack`
Expected: FAIL, `cannot find function `chunks``.

- [ ] **Step 3: Implement**

`chunks` splits the group's text on character boundaries at `EMBED_DESCRIPTION_LIMIT`, and titles each `"<name>"` or `"<name> (i/n)"` when there is more than one.

`into_messages` walks the chunks, keeping a running sum of `title.chars().count() + description.chars().count()`, and starts a new message when adding the next chunk would exceed `MESSAGE_CHARACTER_BUDGET`. A single chunk that exceeds the budget on its own is impossible, since `4096 + 256 < 6000`.

`strip_ansi` is a small state machine over CSI sequences, well under the 100 lines that would justify a dependency.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked stream::pack
git add src/stream/pack.rs && git commit -m "feat: pack embeds to Discord's real character budget"
```

---

### Task 8: stream/mod.rs, the subscribe and flush loop

**Files:**
- Modify: `src/stream/mod.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `shepherd::Live`, `names::Names`, `buffer::Buffer`, `pack`.
- Produces: `stream::Sink` (trait with `async fn send(&self, channel: u64, chunks: Vec<pack::Chunk>) -> Result<(), SinkError>`), `stream::run(live, config, names, sink, stop)`.

- [ ] **Step 1: Write the failing tests against a recording sink**

```rust
/// This dog's own output must not feed back into the channel it writes to.
/// The old code compared against the literal package name at
/// `Discord.ts:73`, so a renamed install logged its own logs.
#[tokio::test]
async fn the_dog_never_streams_its_own_output() {
    let sink = Recording::new();
    let mut state = State::new(own_id(9), &sink);
    state.on_event(BusEvent::LogOut { id: 9, line: "my own line".into() });
    state.on_event(BusEvent::LogOut { id: 1, line: "web line".into() });
    state.flush().await;
    let sent = sink.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].description.contains("web line"));
    assert!(!sent[0].description.contains("my own line"));
}

/// The daemon's bus ring is 1,024 events and it says so when a subscriber
/// falls behind. Neither source repo could report a gap, because PM2's bus
/// had no such signal.
#[tokio::test]
async fn a_dropped_run_is_reported_rather_than_hidden() {
    let sink = Recording::new();
    let mut state = State::new(own_id(9), &sink);
    state.on_event(BusEvent::Dropped { count: 37 });
    state.flush().await;
    let sent = sink.sent();
    assert!(sent[0].description.contains("37"), "{:?}", sent[0].description);
    assert!(sent[0].description.contains("dropped"), "{:?}", sent[0].description);
}

/// A batch Discord refuses on its shape will be refused identically
/// forever. Retrying it silences every line behind it, which is what the
/// old code did at `Discord.ts:68`.
#[tokio::test]
async fn a_rejected_batch_is_dropped_not_retried() {
    let sink = Recording::rejecting(SinkError::BadRequest);
    let mut state = State::new(own_id(9), &sink);
    state.on_event(BusEvent::LogOut { id: 1, line: "web line".into() });
    state.flush().await;
    state.flush().await;
    assert_eq!(sink.attempts(), 1, "the second flush must not resend the refused batch");
}

#[tokio::test]
async fn stderr_and_stdout_go_to_their_own_channels() {
    let sink = Recording::new();
    let mut state = State::with_channels(own_id(9), Some(10), Some(20), &sink);
    state.on_event(BusEvent::LogOut { id: 1, line: "out".into() });
    state.on_event(BusEvent::LogErr { id: 1, line: "err".into() });
    state.flush().await;
    assert_eq!(sink.channel_of("out"), Some(10));
    assert_eq!(sink.channel_of("err"), Some(20));
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked stream::tests`
Expected: FAIL, `cannot find type `State``.

- [ ] **Step 3: Implement**

`run` subscribes to `log.out`, `log.err`, `process.start` and `process.delete`, then drives a `tokio::select!` over the event stream, a `flush` interval, and `Stop`. The `select!` is `biased` with `stop` first, matching shep-log-rotate's `wait`.

`State` holds two `Buffer`s, one per stream, so stdout and stderr keep separate windows and separate channels. A `LogOut` or `LogErr` whose id equals this dog's own is discarded before it reaches a buffer. A `process.start` or `process.delete` triggers a `Live::flock` and a `Names::refresh`.

`BusEvent::Dropped { count }` and a `Lagged` item both push a synthetic line reading `<N> lines dropped: this dog fell behind the shepherd's bus`. A `Lagged` item does not end the stream.

A `SinkError::BadRequest` logs the computed character total and discards the batch. A `SinkError::RateLimited { retry_after }` sleeps and retries that batch once.

An unsubscribed channel (`log_channel` or `err_channel` unset) means that buffer is never filled at all, rather than filled and discarded.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked stream::
git add src/stream/mod.rs src/main.rs
git commit -m "feat: stream sheep log lines into a Discord channel"
```

**Milestone.** At this point the dog is shippable without a bot: `shep adopt ./target/release/shep-discord`, a `[discord]` section naming `token`, `guild_id` and `log_channel`, and log output arrives in the channel.

---

### Task 9: bot/embed.rs, the process embed and its buttons

**Files:**
- Create: `src/bot/mod.rs` (module declarations only for now), `src/bot/embed.rs`

**Interfaces:**
- Produces: `embed::process_embed(&ProcessInfo) -> CreateEmbed`, `embed::process_buttons(&ProcessInfo) -> CreateActionRow`, `embed::CUSTOM_ID_LIMIT: usize = 100`, `embed::parse_custom_id(&str) -> Option<(Verb, u32)>`, `embed::custom_id(Verb, u32) -> String`.

- [ ] **Step 1: Write the failing tests**

```rust
/// Discord caps a custom_id at 100 characters and shep does not constrain
/// a sheep name, so the old scheme at `process.ts:127` is not safe to
/// reuse. `SelectorSpec::Id` exists to receive the numeric form.
#[test]
fn a_custom_id_is_short_whatever_the_sheep_is_called() {
    let id = custom_id(Verb::Restart, u32::MAX);
    assert!(id.chars().count() <= CUSTOM_ID_LIMIT, "{id}");
    assert_eq!(id, "restart:4294967295");
}

#[test]
fn a_custom_id_round_trips() {
    for verb in [Verb::Start, Verb::Stop, Verb::Restart, Verb::Delete, Verb::Flush] {
        assert_eq!(parse_custom_id(&custom_id(verb, 7)), Some((verb, 7)));
    }
}

#[test]
fn a_custom_id_this_dog_did_not_write_is_refused() {
    assert_eq!(parse_custom_id("restart"), None);
    assert_eq!(parse_custom_id("restart:web"), None);
    assert_eq!(parse_custom_id("explode:1"), None);
}

/// An action row holds at most 5 buttons. The row is exactly at the
/// ceiling, so a sixth verb needs a second row rather than a silent 400.
#[test]
fn the_button_row_is_within_what_an_action_row_holds() {
    let row = process_buttons(&info(1, "web"));
    assert!(button_count(&row) <= 5, "an action row holds at most 5 buttons");
}

/// Six of the twelve fields the old embed carried have no source in
/// `ProcessInfo`. Naming them here is the tripwire against somebody
/// reintroducing an "Unknown" row to fill the gap.
#[test]
fn the_embed_carries_no_field_shep_cannot_answer() {
    let rendered = format!("{:?}", process_embed(&info(1, "web")));
    for absent in ["Exec Mode", "Namespace", "Interpreter", "Max Memory", "Autorestart"] {
        assert!(!rendered.contains(absent), "{absent} has no ProcessInfo source");
    }
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked bot::embed`
Expected: FAIL.

- [ ] **Step 3: Implement**

`process_embed` builds a `CreateEmbed` with `Colour` green when `status` is online and red otherwise, a title of the sheep's name, and inline fields: Status, Uptime (`UpDuration` rendering `uptime_ms`), CPU, Memory (`MemSize` rendering `memory_bytes`), Restarts, PID, Sheep ID. Then, only when set: Instance, Lambs, Fold, Smit, Dog, and `dog_stale` when true.

`process_buttons` builds five `CreateButton::new(custom_id(verb, info.id))` with the styles the old code used: Start success, Stop secondary, Restart secondary, Delete danger, Flush primary.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked bot::embed
git add src/bot/ && git commit -m "feat: render a sheep as an embed with action buttons"
```

---

### Task 10: bot/command.rs, the Command trait and registration

**Files:**
- Create: `src/bot/command.rs`

**Interfaces:**
- Produces: `command::Command` (trait: `fn data(&self) -> CreateCommand`, `async fn run(&self, ctx, &CommandInteraction, &State) -> Result<(), Error>`, and provided no-op `autocomplete` and `button`), `command::registry() -> Vec<Box<dyn Command>>`, `command::register(&Http, GuildId, &[Box<dyn Command>]) -> Result<Vec<String>, serenity::Error>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_registered_command_is_admin_gated() {
    // Both source repos gated all three on Administrator. A command that
    // restarts production is not one an unprivileged member gets to try.
    for command in registry() {
        let json = serde_json::to_value(command.data()).expect("json");
        assert!(
            json.get("default_member_permissions").is_some(),
            "{} is not permission gated",
            json["name"]
        );
    }
}

#[test]
fn no_two_commands_share_a_name() {
    let mut names: Vec<String> = registry()
        .iter()
        .map(|c| serde_json::to_value(c.data()).expect("json")["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "a duplicate name silently shadows a command");
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked bot::command`
Expected: FAIL, `cannot find function `registry``.

- [ ] **Step 3: Implement**

The trait carries `data`, a required `run`, and default-empty `autocomplete` and `button`. There is no `modal` hook: neither source repo implements one, and a trait method nothing implements is a method that rots.

`register` calls `GuildId::set_commands` with every `data()`. It runs unconditionally. The `NODE_ENV=development` skip at `register.ts:12` is not ported, because it silently hid newly added commands during exactly the work that adds them.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked bot::command
git add src/bot/command.rs && git commit -m "feat: define the command contract and register it with the guild"
```

---

### Task 11: bot/mod.rs, interaction dispatch, and /system

The first task where the gateway actually comes up.

**Files:**
- Create: `src/bot/interaction.rs`, `src/bot/commands/mod.rs`, `src/bot/commands/system.rs`
- Modify: `src/bot/mod.rs`, `src/main.rs`

**Interfaces:**
- Consumes: `command::Command`, `shepherd::Live`.
- Produces: `bot::State { live: Arc<Live>, config: Arc<Config>, names: Arc<Mutex<Names>>, monitor: Arc<Mutex<Monitor>> }`, `bot::run(config, state, stop)`, `interaction::Handler`.

- [ ] **Step 1: Write the failing tests for dispatch**

```rust
/// serenity's `Interaction` enum replaces the ladder at
/// `interaction.ts:12`, which reconstructed which kind it held from four
/// booleans. This test pins that each arm reaches its own hook.
#[tokio::test]
async fn each_interaction_kind_reaches_its_own_hook() {
    let spy = SpyCommand::default();
    dispatch_kind(&spy, Kind::Command).await;
    dispatch_kind(&spy, Kind::Autocomplete).await;
    dispatch_kind(&spy, Kind::Component).await;
    assert_eq!(spy.calls(), vec!["run", "autocomplete", "button"]);
}

/// The old code ended this path in `.catch(() => {})` at
/// `interaction.ts:55`, so a failure to report a failure vanished.
#[tokio::test]
async fn a_failure_to_report_a_failure_still_reaches_stderr() {
    let spy = SpyCommand::failing();
    let reported = dispatch_with_broken_followup(&spy).await;
    assert!(reported.contains("could not tell the user"), "{reported}");
}

#[tokio::test]
async fn an_unknown_component_id_is_answered_rather_than_ignored() {
    let reply = dispatch_component("something:else").await;
    assert!(reply.contains("not a button this bot wrote"), "{reply}");
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked bot::interaction`
Expected: FAIL.

- [ ] **Step 3: Implement dispatch**

```rust
async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
    match interaction {
        Interaction::Command(command) => { /* defer_ephemeral, look up, run */ }
        Interaction::Autocomplete(command) => { /* look up, autocomplete, no defer */ }
        Interaction::Component(component) => { /* defer_ephemeral, route by custom_id prefix */ }
        Interaction::Ping(_) | Interaction::Modal(_) => { /* nothing this bot answers */ }
    }
}
```

Everything except autocomplete defers ephemerally before the lookup, as both source repos did: Discord gives three seconds, and a `Live::flock` on a busy shepherd can outlast that.

- [ ] **Step 4: Implement `/system`**

```rust
// 131 lines of os.cpus() arithmetic in `services/system.ts` become one
// request. `HostUsage` also carries disk and network rates the old embed
// never had.
let usage = state.live.host_usage().await?;
```

Fields: CPU, Memory (used of total), plus Disk and Network when `disk_bytes_per_second` and `network_bytes_per_second` are `Some`. A `None` `HostUsage` answers "the shepherd is not sampling host usage" rather than rendering zeroes.

- [ ] **Step 5: Implement `bot::run` and wire it into main**

`Client::builder(&config.token, GatewayIntents::GUILDS)` with the handler, then `start()`. `GUILD_MESSAGES` is not requested: this bot reads no message content and an intent nothing uses is an intent to justify at verification time.

`main` now runs two tasks under one runtime: `stream::run` and `bot::run`. The runtime becomes `new_multi_thread` rather than `new_current_thread`, because the gateway and the stream both do real work.

- [ ] **Step 6: Verify against a real guild**

```bash
cargo build --locked
SHEP_DOG_NAME=discord ./target/debug/shep-discord
```

Expected: the bot comes online, `/system` returns an embed.

- [ ] **Step 7: Commit**

```bash
git add src/bot/ src/main.rs
git commit -m "feat: bring up the gateway and answer /system"
```

---

### Task 12: bot/commands/shep.rs, the verbs and autocomplete

**Files:**
- Create: `src/bot/commands/shep.rs`

**Interfaces:**
- Consumes: `shepherd::Verb`, `shepherd::Live`, `embed::process_embed`.
- Produces: `shep::ShepCommand`.

- [ ] **Step 1: Write the failing tests**

```rust
/// The old code marked `name` optional for every verb at `pm2.ts:46` and
/// caught the missing case as a thrown error at the service layer. Discord
/// can refuse it before the interaction is ever sent.
#[test]
fn the_verbs_that_need_a_name_declare_it_required() {
    let json = serde_json::to_value(ShepCommand.data()).expect("json");
    let sub: Vec<_> = json["options"].as_array().expect("options").iter().collect();
    for verb in ["start", "stop", "restart", "reload", "delete", "flush"] {
        let option = sub.iter().find(|o| o["name"] == verb).expect(verb);
        let name_option = option["options"].as_array().expect("sub options")
            .iter().find(|o| o["name"] == "name").expect("name");
        assert_eq!(name_option["required"], true, "{verb} needs a name");
    }
    for verb in ["list", "save", "reopen"] {
        assert!(sub.iter().any(|o| o["name"] == verb), "{verb} missing");
    }
}

#[tokio::test]
async fn autocomplete_offers_the_live_flock() {
    let (live, mut fake) = test_live().await;
    fake.expect(Request::ListFlock).answer(Response::Flock(vec![sample("web"), sample("api")]));
    let offered = ShepCommand.suggestions(&live, "").await;
    assert_eq!(offered, vec!["web", "api"]);
}

#[tokio::test]
async fn autocomplete_answers_empty_rather_than_erroring() {
    // Discord shows nothing for a failed autocomplete either way, and an
    // error here would be logged once per keystroke.
    let (live, mut fake) = test_live().await;
    fake.expect(Request::ListFlock).answer_error("no shepherd");
    assert!(ShepCommand.suggestions(&live, "").await.is_empty());
}

#[tokio::test]
async fn ignore_dogs_hides_dogs_from_the_listing() {
    let (live, mut fake) = test_live().await;
    fake.expect(Request::ListFlock).answer(Response::Flock(vec![sample("web"), dog_sample("bark")]));
    let listed = ShepCommand.flock_for_listing(&live, true).await.expect("ok");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "web");
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked bot::commands::shep`
Expected: FAIL.

- [ ] **Step 3: Implement**

`/shep` takes one subcommand per verb. The six that act on a sheep declare a required, autocompleting `name`. `list`, `save` and `reopen` take none. `reopen` without a name selects `SelectorSpec::All`.

`list` answers with one embed per sheep, and `into_messages`' budget logic from Task 7 applies here too: a flock of thirty sheep does not fit one message.

`Verb::Flush` is reachable only from this file's `flush` subcommand.

- [ ] **Step 4: Run, then commit**

```bash
cargo test --locked bot::commands::shep
git add src/bot/commands/shep.rs && git commit -m "feat: expose shep's verbs as /shep subcommands"
```

---

### Task 13: bot/channel.rs and bot/monitor.rs, the live monitor

**Files:**
- Create: `src/bot/channel.rs`, `src/bot/monitor.rs`, `src/bot/commands/monitor.rs`

**Interfaces:**
- Consumes: `embed::{process_embed, process_buttons, parse_custom_id}`, `shepherd::Live`.
- Produces: `channel::Board` (trait: `async fn post`, `async fn edit`, `async fn delete`, `async fn recent`), `channel::Live` (its serenity implementation), `channel::rediscover(&impl Board, UserId) -> Result<HashMap<u32, MessageId>, Error>`, `monitor::Monitor { messages, task }`, `monitor::update_one`, `monitor::update_all`, `monitor::MonitorCommand`.

`Board` is what lets every test in this task run without a token or a gateway. `CountingChannel` in the tests below is an implementation of it.

- [ ] **Step 1: Write the failing tests**

```rust
/// shep restarts a dog, and the cached message ids live in memory. The id
/// is already encoded in every button this bot wrote, so rediscovery is
/// free. This is what replaces the bulk delete at `utils.ts:37`.
#[test]
fn a_monitor_message_is_recognised_by_the_buttons_it_carries() {
    let message = message_with_buttons(&["restart:7", "stop:7", "delete:7"]);
    assert_eq!(sheep_id_of(&message), Some(7));
}

#[test]
fn a_message_this_bot_did_not_write_is_not_adopted() {
    assert_eq!(sheep_id_of(&message_with_buttons(&[])), None);
    assert_eq!(sheep_id_of(&message_with_buttons(&["something:else"])), None);
}

/// The per-name guard at `monitor.ts:12` is the best idea in either source
/// repo: without it two concurrent updates both see "no message exists"
/// and both post, leaving a duplicate embed nothing will ever clean up.
#[tokio::test]
async fn two_concurrent_updates_for_one_sheep_post_once() {
    let monitor = Monitor::new();
    let sink = CountingChannel::new();
    let (first, second) = tokio::join!(
        monitor.update_one(&sink, &info(1, "web")),
        monitor.update_one(&sink, &info(1, "web")),
    );
    first.expect("ok");
    second.expect("ok");
    assert_eq!(sink.sends(), 1, "the second call must join the first, not race it");
}

#[tokio::test]
async fn a_deleted_sheep_loses_its_monitor_message() {
    let monitor = Monitor::new();
    let sink = CountingChannel::new();
    monitor.update_one(&sink, &info(1, "web")).await.expect("ok");
    monitor.forget(&sink, 1).await;
    assert_eq!(sink.deletes(), 1);
}
```

- [ ] **Step 2: Run and watch fail**

Run: `cargo test --locked bot::monitor bot::channel`
Expected: FAIL.

- [ ] **Step 3: Implement rediscovery**

`rediscover` fetches the last 100 messages in the monitor channel with `ChannelId::messages(GetMessages::new().limit(100))`, keeps those whose author is this bot, and reads the sheep id back out of the first button whose `custom_id` `parse_custom_id` accepts. One hundred is Discord's per-fetch ceiling and the same limit the old code used at `utils.ts:41`.

There is no clear-on-start. It existed to avoid orphans from a previous run, and rediscovery adopts them instead.

- [ ] **Step 4: Implement the monitor**

`Monitor` holds `HashMap<u32, MessageId>` and a per-id in-flight map, so a second `update_one` for the same sheep awaits the first rather than racing it. `update_all` fetches the flock, refreshes `Names`, calls `update_one` for each, and calls `forget` for every cached id no longer in the flock.

The interval is a `JoinHandle` plus a `CancellationToken` rather than a `setInterval` id.

`monitor_interval` in `dogs.toml` starts the monitor at boot. `/monitor start` is a runtime override that does not survive a restart, and its reply says so, because a setting an operator expects to persist and does not is worse than one that never claimed to.

- [ ] **Step 5: Wire process events to the monitor**

`stream::run` already subscribes to `process.start` and `process.delete` for `Names`. Extend to `process.stop`, `process.online`, `process.exit` and `process.restart`, and hand each to `monitor::update_one`, or `forget` on delete. The old code gated this on the monitor being live at `ready.ts:28`; keep that gate.

- [ ] **Step 6: Run, then commit**

```bash
cargo test --locked bot::
git add src/bot/ && git commit -m "feat: keep a live embed per sheep in a monitor channel"
```

---

### Task 14: README, CI, release-plz, and the probe integration test

**Files:**
- Create: `README.md`, `.github/workflows/ci.yml`, `release-plz.toml`, `release-plz-changelog.toml`, `tests/probe.rs`, `.coderabbit.yaml`, `CLAUDE.md`

**Interfaces:**
- Consumes: everything.

- [ ] **Step 1: Write `tests/probe.rs`**

```rust
/// Spawns the binary, because a test that called `probe` directly would
/// pass against a `main` that never calls it. The order is the contract:
/// `shep adopt` reads one line and kills the process group, so a dog that
/// has started connecting is answering late.
#[test]
fn the_binary_answers_the_version_flag_before_it_opens_anything() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shep-discord"))
        .arg("--version")
        .env_remove("SHEP_HOME")
        .env_remove("HOME")
        .output()
        .expect("spawned");
    assert!(output.status.success(), "{:?}", String::from_utf8_lossy(&output.stderr));
    let line = String::from_utf8(output.stdout).expect("utf8");
    assert!(line.contains("shep-protocol:"), "{line}");
    // No $HOME and no $SHEP_HOME. A binary that needed either to answer
    // this has already gone looking for a socket.
}

#[test]
fn the_schema_marks_the_token_as_a_secret() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shep-discord"))
        .arg("--schema")
        .output()
        .expect("spawned");
    let schema: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON schema");
    assert_eq!(schema["properties"]["token"]["x-shep-secret"], true);
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --locked --test probe`
Expected: PASS, two tests.

- [ ] **Step 3: Copy CI and release config from shep-log-rotate**

`.github/workflows/ci.yml` runs the four gates plus `cargo test --locked` on a matrix of 1.88 and stable. `release-plz.toml` and `release-plz-changelog.toml` are copied and have their crate name changed.

- [ ] **Step 4: Write `README.md`**

Sections: what the dog does, what it deliberately leaves to `bark` (with the table from the spec), install and `shep adopt`, the `[discord]` section with every key, the three commands, and a note that `monitor_interval` is what survives a restart.

The `/system` section carries one more line, from the spec at `docs/brainstorming/specs/2026-09-18-shep-discord-design.md:189`: `/system` is built on `Request::HostUsage`, which arrived with protocol 9, so a shepherd older than that refuses the verb by name and `/system` answers with an error rather than an embed. Nothing else in the dog is affected, because a shepherd accepts any peer at or above its own `MIN_SUPPORTED` and that number is still 8. Say the version, not just "an older shepherd".

The README is published prose. Run `humanizer` and then `rin-voice` on it before committing. No em dashes.

- [ ] **Step 5: Write `CLAUDE.md`**

Mirror shep-log-rotate's: the test shape, the four gates, the rules the tests already enforce (`Request::Flush` only from `/shep flush`, no dashes in user-facing strings, redacted `Debug` on secret-holding types, `# Errors` on every fallible `pub fn`), and where things live (`shepherd.rs` is the only module that builds a `Request`).

- [ ] **Step 6: Run the full gate set and commit**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features
cargo +1.88 check --all-targets --all-features --locked
cargo test --locked
cargo llvm-cov --locked --summary-only
git add README.md CLAUDE.md .github/ release-plz.toml release-plz-changelog.toml .coderabbit.yaml tests/
git commit -m "chore: add CI, release config, docs and the probe test"
```

Expected coverage: above 92% of lines, with `main.rs` and `bot/mod.rs` the low files because both need a live connection.

---

## Notes for the executor

**`Request::Flush` has one legal construction site.** Task 12's `flush` subcommand, reached through `Verb::Flush`. shep-log-rotate forbids the request outright with a test that scans its own source; this crate cannot do that, because it has a legitimate caller. If you find yourself adding a second caller, stop: it truncates the log files it names.

**serenity's API was read from the vendored 0.12.5 source on 2026-09-18**, not from memory. `Interaction` has five variants (`Ping`, `Command`, `Autocomplete`, `Component`, `Modal`); `CreateInteractionResponse` has seven; `ChannelId::send_message(cache_http, CreateMessage)` and `ChannelId::messages(cache_http, GetMessages)` are the two call shapes the monitor needs. If a signature does not match, the crate version moved and the plan is what is stale.

**The milestone after Task 8 is real.** If the bot half stalls, a dog that streams logs is worth shipping on its own.
