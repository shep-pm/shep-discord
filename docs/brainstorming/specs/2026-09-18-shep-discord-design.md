# shep-discord design

Status: approved 2026-09-18. Supersedes nothing.

A Discord dog for shep. One binary holding a serenity gateway connection and a shep bus subscription, so an operator can drive the flock from Discord and watch its log output in a channel.

Ports and combines two published TypeScript programs: [`pm2-discord-logger`](https://github.com/TurtIeSocks/pm2-discord-logger) (292 LOC) and [`discord-pm2`](https://github.com/TurtIeSocks/discord-pm2) (1,146 LOC). Both work against PM2 and neither has anything wrong with it; shep replaces their host, so they need a host again. The trace, assessment and old-to-new map that this spec is built on live in `docs/systematic-refactor/refactor-workspace/`.

## Scope, and what bark keeps

shep already ships `bark`, a built-in dog with a `Sink::Discord` webhook, a rule engine and per-subject debounce. Event alerting is bark's and stays bark's. This dog does the two things bark structurally cannot:

| | bark | shep-discord |
|---|---|---|
| lifecycle and threshold alerts | yes | no, deliberately |
| log line streaming | no | yes |
| slash commands, buttons, autocomplete | no | yes |
| live monitor embeds | no | yes |

bark delivers one HTTP POST per firing and appends every firing to a size-capped `barks.jsonl`, so a log firehose through it would rate-limit and would evict real alerts. Log *alerting*, meaning "fire when a line matches a pattern", is alert-shaped and belongs upstream: filed as [shep-pm/shep#341](https://github.com/shep-pm/shep/issues/341).

## Architecture

Two tasks, one process, one config section.

```
                  shep.sock
                      |
              ReconnectingClient
                      |
        +-------------+--------------+
        |             |              |
   subscribe      requests       requests
   log.out            |              |
   log.err            |              |
   process.*          |              |
        |             |              |
     stream/      bot/commands   bot/monitor
        |             |              |
        +------- Discord HTTP / gateway -------+
```

`shepherd.rs` is the only module that builds a `Request`. That mirrors shep-log-rotate's rule about `tick.rs`, and it is what keeps the Discord layer testable without a socket.

### Modules

```
src/
  main.rs         probe, Action, Identity, connect, run
  config.rs       Section, Config, PRINT_CONFIG
  error.rs        Error
  stop.rs         Stop (copied from shep-log-rotate)
  shepherd.rs     Live: the only builder of Request
  names.rs        id to name cache
  stream/
    mod.rs        the subscribe and flush loop
    buffer.rs     bounded buffer, coalescing window
    pack.rs       chunking and 6,000 character packing
  bot/
    mod.rs        Bot, State
    command.rs    Command trait, registry, registration
    interaction.rs
    embed.rs      process embed, button row
    channel.rs    channel resolve, message rediscovery
    monitor.rs    update_one, update_all, Monitor
    commands/
      shep.rs
      monitor.rs
      system.rs
```

`DEFAULT_NAME` is `discord`, matching what `shep adopt shep-discord` picks on its own.

## Startup

`main`'s first line is `shep_client::dogs::probe::<config::Section>(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))`, before the argument parser, for the reason shep-log-rotate's `main.rs` gives: `shep adopt` spawns the binary with `--version` and then `--schema`, reads one line of stdout, and kills the process group. A dog that has started connecting is answering late.

`Identity` is copied from shep-log-rotate unchanged, including the rule that the handshake name comes from `$SHEP_DOG_NAME` and is never guessed. A dog that invents a name gets a refusal recorded against somebody else's dog.

Arguments accepted: none, `--print-config`, and the two probe flags handled before the parser. Anything else is refused with usage text.

## Configuration

One `[discord]` section in `dogs.toml`, served over the socket. Never the environment, for the reason `docs/dogs.md` gives: an environment variable is readable from the process table, inherited by every child, and captured into a crash dump.

| Key | Type | Default | Notes |
|---|---|---|---|
| `token` | String | required | `#[shep(secret)]` |
| `guild_id` | u64 | required | |
| `monitor_channel` | Option\<u64\> | none | unset disables the monitor |
| `monitor_interval` | Option\<UpDuration\> | none | set means the monitor runs from boot |
| `log_channel` | Option\<u64\> | none | unset disables the stdout stream |
| `err_channel` | Option\<u64\> | none | unset disables the stderr stream |
| `flush` | UpDuration | 1s | how often to drain the buffer |
| `coalesce` | UpDuration | 1s | how wide a window joins lines into one embed |
| `buffer_lines` | usize | 2000 | bounded, drops oldest with a visible count |
| `ignore_dogs` | bool | false | hide other dogs from listings, replacing `ignoreModules` |

`flush` and `coalesce` are two keys on purpose. `pm2-discord-logger` had one `buffer_seconds` and its coalescing window ignored it, hardcoded at 1,000 ms at `Discord.ts:96`, so the two could never be tuned apart.

`monitor_interval` has a floor of 15 seconds, carried over from the old 0.25 minute minimum.

The nine per-event booleans from `pm2-discord-logger` are not ported. They configured event alerting, which bark owns.

### Secrets

`token` carries `#[shep(secret)]`, so the schema published to `--schema` marks it and lookout's config pane masks it. `Section` and `Config` get a hand-written `Debug` that redacts it, pinned by an exact-string test, the way bark's `Sink` is at `sinks.rs:29`. A derived `Debug` on a type holding a bot token puts that token in any log line, panic message or error chain that prints it.

`Live` holds a socket path and gets the same treatment.

## The log stream

The half ported from `pm2-discord-logger`, with four defects fixed.

### The loop

Subscribe to `log.out` and `log.err`. A `select!` over the event stream and a `flush` timer replaces the two bus callbacks plus `setInterval` at `app.ts:78`.

`BusEvent::LogOut` carries `{ id, line }` and no name. PM2's bus named the process on every frame, so the old code never needed a lookup. `names.rs` caches id to name off `ListFlock`, refreshes on a `process.start` or `process.delete` event, and falls back to `sheep <id>` rather than dropping a line.

The daemon's bus ring is 1,024 events and tells a slow subscriber it fell behind. `BusEvent::Dropped` and `shep_client::Lagged` both become a visible "N lines dropped" notice in the channel. Neither source repo could report a gap because PM2's bus had no such signal.

### Self-filtering

This dog's own output must not feed back into the channel it writes to. `pm2-discord-logger` compared the frame's process name against the literal string `pm2-discord-logger` at `Discord.ts:73`, so a renamed install stopped self-filtering and logged its own logs.

Filter on this dog's own numeric id, learned from the flock at startup by matching `$SHEP_DOG_NAME`. A rename cannot break it.

### Buffering and coalescing

`buffer.rs` holds raw lines with their arrival time. `drain` shifts the oldest, then absorbs every following line inside `[at, at + coalesce)`, and repeats until the buffer is empty.

Two changes from `collectLogs` at `Discord.ts:87`. The window reads config rather than a hardcoded constant. And `drain` returns every complete group rather than one, because the old code handled exactly one group per flush and a backlog therefore never cleared.

The buffer is bounded at `buffer_lines`. Over the cap it drops oldest and records the count, which surfaces in the channel. The old one grew without limit.

### Packing

This is where the old code was wrong in a way that mattered, and the correction is the reason this section exists.

Discord's [embed limits](https://discord.com/developers/docs/resources/message#embed-object-embed-limits):

- `description`: 4,096 characters
- `title`: 256 characters
- combined sum across `title`, `description`, `field.name`, `field.value`, `footer.text` and `author.name`, over every embed on one message: 6,000 characters

The old code chunked at 4,095 characters and then sent every pending embed in one call at `Discord.ts:67`. Two full chunks is 8,192 characters, which is a 400. Worse, `this.messages = []` at `Discord.ts:68` runs only after a successful send, so a rejected batch is retried identically on every tick, forever.

`pack.rs` therefore:

1. Chunks a coalesced group at 4,096 characters, suffixing `(i/n)` when there is more than one, as the old code did.
2. Packs embeds into a message until the running total of counted characters would exceed 6,000, then starts a new message. The binding constraint is the character sum, not an embed count.
3. Strips ANSI on the way in.

A 400 on a packed message logs the message's computed size and drops that batch. Dropping is the fix: a batch Discord refuses on its shape will be refused identically forever, and retrying it silences every line behind it.

A 429 backs off on `Retry-After`, which serenity handles. Discord's per-channel message rate limits are dynamic rather than a documented constant, so nothing here hardcodes a rate.

## The bot

### Commands

The `Command` trait carries `data`, a required `run`, and optional `autocomplete` and `button`. The `modal` hook from `discord-pm2`'s `types.ts:12` has no implementor in either source repo and is not ported on spec.

Registration PUTs every command's JSON to the guild command route at startup. The `NODE_ENV=development` skip at `register.ts:12` is dropped: it silently hid newly added commands during development, which is the opposite of useful.

All three commands keep the administrator permission gate both source repos had.

**`/shep <command> [name]`**, renamed from `/pm2`. Verbs follow shep's own rather than PM2's:

| Discord verb | Request |
|---|---|
| `start`, `stop`, `restart`, `reload`, `delete` | the matching verb with a `SelectorSpec` |
| `flush` | `Request::Flush` |
| `list` | `Request::ListFlock` |
| `save` | `Request::SaveRoll`, was `dump` |
| `reopen` | `Request::Reopen`, was `reloadlogs` |

`name` is required for the verbs that need one, declared in the command schema. The old code marked it optional for every verb and caught the missing case as a thrown error at the service layer at `pm2.ts:157`.

Autocomplete over the live flock is kept as it was.

`Request::Flush` truncates the log files it names. shep-log-rotate forbids building it at all, enforced by a test that scans its own source. Here it is a verb an operator explicitly typed, so it is allowed, and it must stay reachable only from `/shep flush`. No internal path may construct it.

**`/monitor start|update|stop`** toggles the live monitor for this process's lifetime. It does not write config.

**`/system`** answers from `Request::HostUsage`, which supplies `cpu_percent`, `memory_used_bytes` and `memory_total_bytes`, plus `disk_bytes_per_second` and `network_bytes_per_second` that the old embed never had. `discord-pm2`'s `services/system.ts`, 131 lines of `os.cpus()` delta arithmetic with a module-global race at `system.ts:27`, is deleted.

### Interaction dispatch

serenity's `Interaction` enum replaces the name-derivation ladder at `interaction.ts:12`, which had to reconstruct which kind of interaction it held from four booleans. Everything except autocomplete defers ephemerally up front, as before.

A failure while reporting a failure gets a stderr line. The old code ended that path in `.catch(() => {})` at `interaction.ts:55`.

### The process embed

Six of the twelve fields in `discord-pm2`'s embed have no source in shep's `ProcessInfo`: `version`, `namespace`, `exec_mode`, `max_memory_restart`, `autorestart` and `interpreter`. They are dropped rather than faked.

Kept: status, uptime, cpu, memory, restarts, the OS pid, and shep's own sheep id. Added from shep: `instance`, `lambs`, `fold`, `smit`, `dog`, and `dog_stale` when set. Colour by status carries over.

### Buttons

`custom_id` is `"<verb>:<id>"` on the numeric `ProcessInfo.id`.

The old scheme put the process name in the id at `process.ts:127`, on a comment asserting that PM2 names cannot contain colons. Discord caps a `custom_id` at 1 to 100 characters and shep does not constrain a sheep name, so that scheme is not safe to reuse. `SelectorSpec::Id(u32)` exists to receive the numeric form.

An action row holds at most 5 buttons. The current row is exactly 5, so it is at the ceiling and a sixth verb needs a second row.

## The monitor

One message per sheep in `monitor_channel`, each an embed plus a button row, refreshed on a timer and on bus events.

### State across a restart

shep restarts a dog. The interval and the cached message ids live in memory, so both are lost.

On boot, rediscover: fetch the last 100 messages in `monitor_channel`, keep the ones this bot authored, and parse the sheep id back out of each message's button `custom_id`. One hundred is Discord's own per-fetch ceiling and the same limit the old code used at `utils.ts:41`. That rebuilds the cache for free, since the id is already encoded there, and it is self-healing after any restart.

Two alternatives were considered and rejected. `kv.json` caps a value at 4,096 bytes and its own module documents it as not a blob store. Writing the interval back with `Request::SetDogConfig` is worse: [`dispatch.rs:601`](https://github.com/shep-pm/shep/blob/main/crates/shep-daemon/src/rpc/dispatch.rs) replaces the whole section, so persisting one number means round-tripping the bot token through a write path, where a bug truncates the operator's config.

`monitor_interval` in `dogs.toml` is what survives a restart. `/monitor start` is a runtime override that does not.

### Clearing the channel

`discord-pm2` bulk-deleted the bot's last 100 messages on every `/monitor start`, at `utils.ts:37`. That existed to avoid orphaned messages from a previous run. Rediscovery adopts them instead, so the clear is dropped rather than made configurable.

### Concurrency

The per-name in-flight guard at `monitor.ts:12` is the best idea in either source repo and carries over: two concurrent updates for one sheep cannot both decide no message exists and both post. It becomes a per-name entry in a shared map.

The `setInterval` handle becomes a `JoinHandle` plus a cancellation token.

## Error handling

Nothing is fatal except a signal and a refused handshake, copying shep-log-rotate's `poll`. A refused handshake is protocol skew that no amount of reconnecting fixes, so the process exits and lets the shepherd restart it from disk.

Everything else is reported and retried on the next interval. The shepherd restarting underneath a dog is ordinary rather than exceptional.

Every fallible `pub fn` carries a `# Errors` section. Errors implement `core::error::Error`, never `std::error::Error`. `#![forbid(unsafe_code)]` at the crate root.

No em dash or en dash in anything printed for a person. `test_support::assert_no_dashes` is copied from shep-log-rotate and applied to every user-facing string, because a terminal that cannot render one prints a replacement character in the middle of a message somebody is reading while already confused.

## Testing

Neither source repo has a single test. The target is shep-log-rotate's tier, above 92% of lines.

The pure modules carry most of it: `buffer` (the coalescing window, the bounded drop), `pack` (4,096 chunking, 6,000 packing, the `(i/n)` suffix, the drop-on-400 rule), `names`, `embed`, `config`.

`tests/probe.rs` spawns the binary to pin that `probe` answers before the argument parser reaches a flag. A test that called `probe` directly would pass against a `main` that never calls it.

Gateway and channel I/O sit behind a trait, so the command layer tests without a socket or a token. `shep-client`'s `test-support` feature binds a fake shepherd for the `shepherd.rs` tests.

## Packaging

Follows shep-log-rotate, which is the only other third-party dog.

- Edition 2024, `rust-version = "1.88"`, matching shep's own MSRV
- `MIT OR Apache-2.0`
- Published to crates.io through release-plz
- `shep-client` by version, floor 0.7.3, the only path to shep-core. Never a second direct dependency: `shep_client::shep_core`
- `[profile.dev] debug = "line-tables-only"` and `[profile.dev.package."*"] debug = false`
- `[profile.release] lto = "thin"`, `codegen-units = 1`. Not `strip`, and not `panic = "abort"`: symbols are what a profiler names frames with, and `shep-client` re-raises a panicked task with `resume_unwind`
- The four CI gates: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features`, and `cargo +1.88 check --all-targets --all-features --locked`

### serenity

`serenity = { version = "0.12", default-features = false, features = ["client", "gateway", "model", "builder", "rustls_backend"] }`.

No `cache`, since this dog holds its own state. No `framework` or `standard_framework`, which serve prefix commands rather than slash commands.

Measured 2026-09-18 against a dog base of `shep-client`, `schemars`, `tokio`, `toml` and `serde`, which is 100 crates on its own:

| | serenity 0.12.5 | twilight 0.17.1 |
|---|---|---|
| crates after adding | 202, so +102 | 144, so +44 |
| duplicate TLS stacks | 2 rustls: 0.22 through `tokio-tungstenite 0.21`, 0.23 through `reqwest 0.12` | none |
| MSRV | 1.74 | 1.89 |
| stars | 5,607 | 873 |
| last push | 2026-09-16 | 2026-08-30 |
| downloads, 90 days | 1,171,017 | 131,671 |
| open issues | 61 | 72 |

serenity costs more than twice the crates and compiles two rustls versions into the binary, which is its own internal inconsistency rather than a conflict with shep: its gateway sits on a stale `tokio-tungstenite 0.21` while its HTTP is on a current `reqwest`.

It is still the choice. The gateway is the hard part of this crate, covering heartbeats, resume, session state, sharding and rate limits, and serenity has roughly nine times the field testing on exactly that. twilight's MSRV of 1.89 also sits above shep's pinned 1.88, so adopting it means this dog diverges from shep's toolchain floor for a dependency-count win.

bark hand-rolls HTTP/1.1 over `tokio-rustls` to avoid `reqwest`, and that reasoning does not transfer here. bark sends one POST, where hand-rolling is about a hundred lines. A gateway client is not.

The bot layer sits behind the `Command` trait and a channel I/O trait, so a later swap to twilight is contained to `bot/mod.rs` and `bot/channel.rs`. If binary size or the double TLS stack becomes a real problem, that is the escape hatch.

## Out of scope

- Event alerting. bark owns it.
- Log alerting on a pattern. That belongs in bark, filed as [shep-pm/shep#341](https://github.com/shep-pm/shep/issues/341).
- Webhook delivery for the log stream. Channel ids keep the credential count at one and the bot is already connected. Webhooks would give the stream its own rate-limit budget, which is worth revisiting only if the stream starts starving command replies.
- Modals.
- Multi-guild support. One `guild_id`, as both source repos had.
- Sharding. A single shard covers well past the scale a self-hosted process manager reaches.
