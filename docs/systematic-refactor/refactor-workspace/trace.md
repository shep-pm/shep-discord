# 🦆 Trace: `pm2-discord-logger` + `discord-pm2`

**In one sentence:** Two separate TypeScript programs bolted onto PM2 — one pushes PM2's log lines and lifecycle events out to Discord webhooks, the other is a Discord gateway bot that drives PM2 from slash commands and keeps a live embed per process in a monitor channel.

> **Traced:** both repos, every public symbol · **Voice:** Matched · **Direction:** Bottom-up (building blocks, then the two flows) · **Depth:** Function-level (1,438 LOC combined, well under the 10k threshold)

Both repos are **[UNTESTED]** — neither has a single test file. `pm2-discord-logger` is **[STALE]**: last commit 2024-03-31, 7 commits total. `discord-pm2` was last touched 2026-05-22 across 27 commits; `src/discord/commands/pm2.ts` is its **[HOT]** file at 11 commits.

---

## The building blocks

### `pm2-discord-logger` — 3 files, 292 LOC

| Symbol | Anchor | What it does alone |
|---|---|---|
| `Config` | `src/types.ts:1` | The PM2 module config: three webhook URLs (`log_url`, `error_url`, `event_url`), a `buffer_seconds`, and nine booleans, one per PM2 event kind, that gate whether that event is reported. |
| `Data` | `src/types.ts:72` | One PM2 bus frame: `{ process: { name, version?, restart_time?, unstable_restarts? }, event?, data, at }`. |
| `Role` | `src/types.ts:98` | `'log' \| 'error' \| 'event'` — which of the three webhooks a client owns. |
| `DiscordLogger` | `src/Discord.ts:11` | Extends discord.js `WebhookClient`. Holds a pending-embeds array, a raw-line buffer, its role, and a colour fixed by role (green / red / blue, `Discord.ts:22`). |
| `DiscordLogger.getTitle` | `src/Discord.ts:28` | `"<name> v<version> - <event>"`, dropping `version` when it is absent or the literal `'N/A'`, dropping the event suffix when there is no event. |
| `DiscordLogger.ensureString` | `src/Discord.ts:36` | `JSON.stringify` anything that isn't already a string. |
| `createMessage` | `src/Discord.ts:40` | Splits a description into 4,095-char chunks (`Discord.ts:47`) and pushes one embed per chunk, suffixing `(i/n)` when there is more than one. |
| `sendMessages` | `src/Discord.ts:64` | Calls `collectLogs`, then POSTs every pending embed in one webhook `send` and clears the array. |
| `pushToBuffer` | `src/Discord.ts:72` | Drops frames from its own process by name (`Discord.ts:73`), strips ANSI, and appends `{ process: { name }, data, at: Date.now() }`. |
| `collectLogs` | `src/Discord.ts:87` | Shifts one buffered line, then greedily absorbs every following line whose `at` falls inside a 1,000 ms window, joins them with newlines, and hands the result to `createMessage`. |

### `discord-pm2` — 17 files, 1,146 LOC

**Services (PM2 and OS facts):**

| Symbol | Anchor | What it does alone |
|---|---|---|
| `Process` | `src/services/pm2.ts:42` | Flat view of one PM2 process: name, cpu, memory, uptime, planned/unplanned restarts, status, instances, autorestart, interpreter, maxMemoryRestart, execMode, version, module, namespace, pmId, pid. |
| `toProcess` | `src/services/pm2.ts:67` | Normalizes a raw `pm2.ProcessDescription` into `Process`, defaulting every missing field. Shared by list and describe so both normalize identically. |
| `shouldKeep` | `src/services/pm2.ts:91` | Hides PM2's own modules when `ignoreModules` is set. |
| `getProcessList` | `src/services/pm2.ts:99` | `pm2.list`, deduped by name (clustered apps appear once per instance, first wins — `pm2.ts:107`), then filtered by `shouldKeep`. |
| `getProcess` | `src/services/pm2.ts:125` | `pm2.describe(name)`, first instance only. |
| `executeCommon` | `src/services/pm2.ts:150` | `start`/`stop`/`restart`/`delete`/`flush` by name; returns a past-tense sentence from `PAST_TENSE_MAP` (`pm2.ts:20`). |
| `executeReload` | `src/services/pm2.ts:175` | `pm2.reload` with `updateEnv: true`, split out because its callback shape differs. |
| `executeDump` | `src/services/pm2.ts:196` | `pm2.dump` — the `pm2 save` equivalent. |
| `executeReloadLogs` | `src/services/pm2.ts:216` | `pm2.reloadLogs` — reopen file handles after an external logrotate. |
| `getCPU` | `src/services/system.ts:34` | Aggregate CPU % from the delta in `os.cpus()` times since the previous call, against a module-level `lastSnapshot` (`system.ts:27`). |
| `getMemory` / `formatMemory` | `src/services/system.ts:53`, `:64` | `os.totalmem`/`os.freemem`; bytes rendered as GB/MB/KB/bytes. |
| `getFormattedUptime` | `src/services/system.ts:87` | Seconds to `"2 days, 3 hours"`, suppressing minutes once days is non-zero and seconds once hours is. |
| `getEmbed` | `src/services/system.ts:112` | A three-field `System Status` embed: CPU, Memory, Uptime. |
| `log` / `HELPERS` / `logToDiscord` | `src/services/logger.ts:5`, `:7`, `:33` | A `loglevel` logger with a chalk icon and timestamp prefix; `logToDiscord` posts plain content to a channel id. |

**Discord layer:**

| Symbol | Anchor | What it does alone |
|---|---|---|
| `Command` | `src/types.ts:12` | The contract every command satisfies: `data` (a `SlashCommandBuilder`), a required `run`, optional `autoComplete` / `modal` / `button`. |
| `client.ctx` | `src/types.ts:22` | A module augmentation hanging the command collection and the monitor state (`messages`, `channel`, `interval`) off discord.js's `Client`. |
| `getProcessEmbed` | `src/discord/embeds/process.ts:37` | Twelve inline fields per process, green when online and red otherwise. |
| `getProcessButtons` | `src/discord/embeds/process.ts:122` | One button per `PROCESS_INPUTS` except Reload, custom id `"<id>:<verb>:<name>"` using `CUSTOM_ID_DELIMITER` (`process.ts:20`). |
| `getMonitorChannel` | `src/discord/utils.ts:9` | Resolves `monitorChannel` through the guild, returning `null` at every failure with a log line rather than throwing. |
| `clearMonitorChannel` | `src/discord/utils.ts:37` | Deletes the bot's own last 100 messages, bulk under 14 days and one-by-one past it. |
| `deleteMonitor` | `src/discord/utils.ts:70` | Deletes one process's monitor message and forgets it. |
| `updateMonitor` | `src/discord/monitor.ts:22` | Edits the existing monitor message for a process, or sends a new one and caches it. Guarded by a per-name `inFlight` map (`monitor.ts:12`) so two concurrent calls cannot both decide no message exists. |
| `updateAll` | `src/discord/monitor.ts:70` | `updateMonitor` across the whole list, in parallel. |
| `pm2Command` | `src/discord/commands/pm2.ts:24` | `/pm2 <command> [name]` with autocomplete over live process names plus `all`. Admin-gated. |
| `monitor` | `src/discord/commands/monitor.ts:10` | `/monitor start\|update\|stop`, plus the `button` handler that routes embed buttons back into `executeCommon`. Minimum interval 0.25 min (`monitor.ts:8`). |
| `system` | `src/discord/commands/system.ts:6` | `/system` — posts `getEmbed()`. |
| `register` | `src/discord/register.ts:11` | PUTs every command's JSON to the guild command route. Skipped entirely under `NODE_ENV=development` (`register.ts:12`). |
| `startDiscord` | `src/discord/client.ts:9` | Builds the client, populates `ctx`, wires every exported event handler, logs in, resolves the monitor channel, installs SIGTERM/SIGINT shutdown. |

---

## The walkthrough

### Flow A — a log line reaches Discord (`pm2-discord-logger`)

1. **`io.initModule` hands back the PM2 module config** — `src/app.ts:6`
   The whole program lives inside this callback. `io.getConfig()` returns the values an operator set with `pm2 set pm2-discord-logger:<key>`.

2. **`buffer_seconds` is clamped** — `src/app.ts:14`
   Anything outside `0 < n < 5` silently becomes `1`. 🤫 A deliberate `buffer_seconds = 10` is discarded without a word.

3. **Up to three webhook clients are built** — `src/app.ts:19`
   One per configured URL. An unset URL means that whole category is never reported, which the README states outright.

4. **`pm2.launchBus` opens the PM2 event bus** — `src/app.ts:28`
   ⏳ Everything after this is callback-driven. A bus error exits the process.

5. **`log:out` and `log:err` frames go into a buffer, not out to Discord** — `src/app.ts:36`, `:43`
   Each calls `pushToBuffer`, which drops its own process's frames by name comparison (`Discord.ts:73`) — the one thing standing between this module and an infinite loop of logging its own logs. 🤫 The guard is a hardcoded string equal to the package name, so a renamed install stops self-filtering.

6. **`pm2:kill` and `process:exception` are sent as titled events** — `src/app.ts:51`, `:59`
   Straight to `createMessage` with a synthesized title. Note these two bypass the per-event config booleans entirely — only `process:event` consults them.

7. **`process:event` is gated on the config** — `src/app.ts:67`
   `conf[data.event]` decides. 🔀 A falsy entry drops the event. The description is a two-line restart summary rather than the frame's own payload.

8. **A timer drains all three clients every `buffer_seconds`** — `src/app.ts:78`
   ⏳ `Promise.allSettled` over the three, so one failing webhook does not stop the others. ⚠️ This is the only network write in the program.

9. **`collectLogs` coalesces a 1-second window** — `src/Discord.ts:87`
   One shift, then absorb every following entry inside `[at, at+1000)`. 🤫 The window is hardcoded at 1000 ms and does **not** follow `buffer_seconds`, so a 4-second buffer still coalesces in 1-second groups — and only one group per tick, because `collectLogs` is called once per `sendMessages`. A backlog drains at one group per interval.

10. **`createMessage` chunks and `send` posts** — `src/Discord.ts:40`, `:67`
    Chunks at 4,095 chars. ⚠️ Nothing caps how many embeds go in one `send`; Discord refuses more than 10 per message, so a burst producing 11+ chunks throws, and 🤫 the rejection is swallowed by `allSettled` at `app.ts:79` with the embeds already cleared or not depending on where it threw — `this.messages = []` at `Discord.ts:68` runs only on success, so a failed send retries the same batch forever.

### Flow B — an operator types `/pm2 restart web` (`discord-pm2`)

1. **`src/index.ts:9` refuses to boot without `token`, `clientId`, `guildId`**
   Then `register()` PUTs the command definitions, and `startDiscord()` runs.

2. **`register` is skipped in development** — `src/discord/register.ts:12`
   🤫 `NODE_ENV=development` returns before the PUT, so a new command silently never appears while developing; `bun register` exists to do it by hand.

3. **`startDiscord` wires the client** — `src/discord/client.ts:9`
   Commands become a `Collection` keyed by name (`client.ts:15`), every exported event handler is invoked with the client, then `login`. The monitor channel is resolved *after* login (`client.ts:34`) because it needs a live guild.

4. **`ready` fires and opens the PM2 bus** — `src/discord/events/ready.ts:19`
   ⏳ Sets the "Watching processes" activity, then `pm2.launchBus`. Every `process:event` either deletes a monitor message (on `delete`) or refreshes it (on start/restart/stop/online/exit — `ready.ts:8`). 🔀 The whole handler no-ops unless `client.ctx.monitor.interval` is set, so bus-driven refresh only happens while the monitor is running.

5. **The interaction arrives at one handler** — `src/discord/events/interaction.ts:7`
   The name is derived differently per interaction kind (`interaction.ts:12`) — for a button it is the customId's first `:` segment. Everything except autocomplete is deferred ephemerally up front (`interaction.ts:21`).

6. **The command is looked up and dispatched** — `src/discord/events/interaction.ts:24`
   🔀 Four branches, one per interaction kind. ⚠️ Errors are caught, logged, and answered with a generic `An error has occurred with input: <name>`; the `.catch(() => {})` on the follow-up means a failed error report is 🤫 silent.

7. **`pm2Command.run` branches on the verb** — `src/discord/commands/pm2.ts:62`
   `list` posts an embed per process; `dump` and `reloadlogs` take no name; `reload` goes to `executeReload` and everything else to `executeCommon`. ⚠️ `executeCommon` rejects when `name` is missing (`pm2.ts:157`) — that is the only validation, so `/pm2 restart` with no name is caught as a thrown error rather than refused by the command schema.

8. **The PM2 call runs and a sentence comes back** — `src/services/pm2.ts:159`
   `pm2[command](name, cb)` — the verb indexes into the pm2 module directly.

9. **The reply is an ephemeral follow-up** — `src/discord/commands/pm2.ts:89`
   Plain text, `"restarted web"`.

### Flow C — the live monitor (`discord-pm2`)

1. **`/monitor start` clears the channel first** — `src/discord/commands/monitor.ts:66`
   ⚠️ `clearMonitorChannel` deletes the bot's own last 100 messages in that channel before anything is posted.

2. **`updateAll` posts one embed + button row per process** — `src/discord/monitor.ts:70`
   Each is cached in `client.ctx.monitor.messages` by process name.

3. **An interval is installed** — `src/discord/commands/monitor.ts:74`
   Minimum 0.25 minutes. 🤫 The interval handle lives only in memory; a restart loses the monitor and leaves its messages behind in the channel.

4. **A button press re-enters through the same interaction handler** — `src/discord/commands/monitor.ts:36`
   Splits the customId, runs `executeCommon`, refreshes that one monitor message, then deletes its own reply.

---

## Where the duck would squint 🦆

- **`src/Discord.ts:68`** — `this.messages = []` runs only after a successful `send`. A webhook that keeps failing (rate limit, >10 embeds, revoked URL) retries the identical growing batch every tick forever.
- **`src/Discord.ts:96`** — the coalescing window is a hardcoded `1000` while the flush interval is `buffer_seconds * 1000`. The two were clearly meant to match and do not.
- **`src/Discord.ts:87`** — `collectLogs` handles exactly one group per flush. Under sustained output the buffer grows without bound.
- **`src/Discord.ts:73`** — self-filtering compares against the literal string `'pm2-discord-logger'`. Rename the process and the module logs its own logs.
- **`src/app.ts:14`** — a config value outside the accepted range is replaced with the default, silently.
- **`src/app.ts:51`, `:59`** — `pm2:kill` and `process:exception` ignore the `kill` and `exception` config booleans that exist for them. Only `process:event` is gated.
- **`src/services/system.ts:27`** — `lastSnapshot` is module-global mutable state. Two callers of `getCPU()` in the same tick means the second reads a near-zero delta.
- **`src/discord/events/interaction.ts:55`** — the error follow-up ends in `.catch(() => {})`. A failure to report a failure vanishes.
- **`src/discord/commands/pm2.ts:46`** — `name` is `setRequired(false)` for every verb including `start`/`stop`/`restart`, so missing-name is caught at the service layer as an exception rather than refused by Discord.
- **Both repos** — zero tests.

---

## So the whole point is…

Two halves of the same idea, never joined. `pm2-discord-logger` is a fire-and-forget pipe: PM2's bus in, three webhooks out, with a small buffer in front and a few coalescing bugs inside it. `discord-pm2` is the other direction: a gateway bot that reads PM2 and drives it, with slash commands, autocompleted process names, per-process embeds with action buttons, and a monitor channel that refreshes on a timer and on bus events. The pipe half has no interactivity and the bot half has no log streaming; an operator who wants both runs both, configures Discord twice, and gets two different renderings of the same process.
