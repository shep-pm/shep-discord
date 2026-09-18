# Refactor goals

## What is driving this

**Language and runtime change.** TypeScript on Node/Bun, against PM2, becomes Rust against shep. Both source repos exist only because PM2 is their host; shep replaces PM2, so they have no host any more.

**Combine two programs into one.** An operator who wants both today runs two processes, configures Discord twice, and gets two different renderings of the same process. One dog, one config section, one rendering.

**Dependency reduction, in one direction only.** `services/system.ts` (131 LOC of `os.cpus()` arithmetic) is replaced outright by `Request::HostUsage`. `services/pm2.ts` (227 LOC of callback-wrapping) is replaced by typed requests. Against that, `serenity` is a large tree going the other way, and that is a deliberate trade (below).

**Testability.** Neither source repo has a single test. shep-log-rotate's unit tier sits above 92% of lines. That is the bar.

## Decisions taken

| Question | Answer | Why |
|---|---|---|
| Scope | Gateway bot + log streaming. No event alerting. | `bark` already ships `Sink::Discord` with a rule engine and debounce. Duplicating it means two rule engines that drift. |
| Where log *alerting* goes | `bark`, upstream | Rule-shaped, debounce-shaped, and serves the Slack and Json sinks too. Filed as [shep-pm/shep#341](https://github.com/shep-pm/shep/issues/341). |
| Where log *streaming* goes | Here | Firehose-shaped. Needs batching, coalescing and 4,095-char chunking that bark's one-POST-per-firing path has no room for. |
| Discord library | `serenity` | Closest shape to discord.js, so the port reads close to 1:1. Heavy tree, accepted: the gateway is heartbeats, resume, session state, sharding and rate limits, and hand-rolling it the way bark hand-rolls its one-shot POST would be weeks and a bug source. |
| The 6 embed fields with no source | Dropped, replaced with shep's own | `ProcessInfo` has no `version`, `namespace`, `exec_mode`, `max_memory_restart`, `autorestart` or `interpreter`. It has `lambs`, `instance`, `smit`, `fold`, `dog`, `handshook`, `dog_stale`. The embed becomes shep-native rather than a PM2 embed with holes. |
| Secrets | `dogs.toml` with `#[shep(secret)]` | What `docs/dogs.md` argues for and what bark does. Config rides the socket, never the environment. |

## Constraints

**Breaking changes: not applicable.** New crate, no users. Neither source repo is being kept alive: `pm2-discord-logger` last shipped 2024-03-31, `discord-pm2` is `"private": true` and was never published.

**Migration: none.** Nobody converts a PM2 module into a shep dog in place. An operator adopts the binary with `shep adopt` and writes a fresh `[discord]` section.

**Team: Rin, working with an assistant.** Same shape as shep-log-rotate.

**Packaging follows shep-log-rotate exactly** unless she says otherwise: edition 2024, `rust-version = "1.88"`, `MIT OR Apache-2.0`, published to crates.io through release-plz, `shep-client` by version as the only path to shep-core, `#![forbid(unsafe_code)]`, the four CI lint gates, and the `[profile.dev]` / `[profile.release]` blocks.

## Still open

These do not block the map. They are named where they land.

- **Monitor state across a restart.** shep restarts a dog; the interval handle and the cached message ids are in memory. Three candidates: rediscover from the channel's own messages on boot, persist through `kv.json` (4 KiB value cap, documented as not a blob store), or make the monitor a `dogs.toml` setting that `/monitor start` writes with `Request::SetDogConfig`. The third is the most shep-native and the most likely to need a brainstorm.
- **Whether `/monitor start` keeps clearing the channel.** It bulk-deletes the bot's last 100 messages today. Defensible for a dedicated channel, destructive if pointed at a shared one.
- **Crate and dog name.** `shep-discord` as the crate, `discord` as the `DEFAULT_NAME` section, matching what `shep adopt shep-discord` would pick on its own.
