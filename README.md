<p align="center">
  <img src="assets/gray-logo.svg" alt="gray" width="96">
  <img src="assets/modelcontextprotocol.svg" alt="modelcontextprotocol" width="96">
</p>
<h1 align="center">gray-mcp</h1>
<p align="center">Model Context Protocol for gray — one binary, client and server.</p>
<p align="center">
  <a href="https://github.com/vstaln/gray-mcp/blob/main/LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-7aa2f7.svg">
  <img alt="rust" src="https://img.shields.io/badge/built%20with-rust-orange.svg">
</p>

[Model Context Protocol](https://modelcontextprotocol.io) for
[gray](https://github.com/vstaln/gray), as a sidecar plugin (wire v1.3).
One binary, both directions:

- **Client**: servers listed in `~/.gray/mcp.json` and the project's
  `.mcp.json` become gray tools named `mcp__<server>__<tool>`, published
  live — tools appear when a server connects, vanish when it drops, and a
  server's `tools/list_changed` refreshes the model's tool set between
  turns. Image and blob results reach the model as media, not text.
- **Server**: `gray mcp serve` is a stdio MCP server exposing gray itself
  (`gray_prompt`, `gray_sessions`, `gray_session_read`), so Claude
  Desktop, Cursor or another gray can drive the agent.

Three surfaces, one argument syntax:

- **Tools `mcp__*`** — the agent calls them like built-ins.
- **`/mcp …`** inside a session, over `command/run`.
- **`gray mcp …`** on the command line (the host `exec`s this binary, so
  it works with no agent running).

Needs a gray build that speaks plugin protocol 1.3 (`gray plugin check`
tells you).

## Install

```sh
cargo install --path . --locked
gray plugin check ~/.cargo/bin/gray-mcp
gray plugin install ~/.cargo/bin/gray-mcp
```

`manifest` answers the install probe, so one install wires the sidecar,
`/mcp` and `gray mcp`. The plugin declares one capability, `host.ask`
(ask you a blocking question in the middle of a tool call) — grant it at
the install prompt so project-server consent can actually ask you;
`gray plugin capabilities mcp` shows the state. Ungranted, project
servers are denied without asking.

## Configuration

`~/.gray/mcp.json` is the user file; `<cwd>/.mcp.json` is the project
file (all `~/.gray` paths follow `$GRAY_HOME`). Same shape as Claude
Code's `mcp.json`:

```json
{
  "mcpServers": {
    "fs":  {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]},
    "api": {"url": "https://mcp.example.com/mcp",
            "headers": {"Authorization": "Bearer ${MCP_TOKEN}"}},
    "opt": {"command": "my-server", "timeout": 60, "disabled": false}
  }
}
```

- Each entry is either **stdio** (`command`, optional `args`, `env`,
  `env_file`) or **streamable HTTP** (`url`, optional `headers`) — exactly
  one of `command` / `url`.
- `env_file` (stdio only): path to a `KEY=VAL` dotenv-style file merged
  into the spawned process environment — secrets live in the file, not in
  this JSON. `env` keys win on conflicts; `~` expands to `$HOME`.
- `timeout`: seconds per call, default 120, clamped to 1–300.
- `disabled`: keep the entry, never connect.
- `${VAR}` expands from the environment in `command`, `args`, `env`
  values, `env_file`, `url` and `headers` values. An unset variable invalidates that
  entry: it is skipped with a warning, nothing else breaks.
- Both files merge; project wins on a name clash. Invalid entries are
  skipped with a warning, never fatal.

## Consent

User-file servers run unconditionally. A server from a project
`.mcp.json` runs only after you allow it once: on session start the
plugin asks through `host/ask` (up to 3 servers per dialog), `Allow`
starts it and is remembered, `Deny` skips it for the session. Consent is
stored in `~/.gray/mcp/consent.json` as SHA-256 keys of
`project dir + server name + canonical unexpanded entry` — editing the
entry asks again, and secrets pulled in by `${VAR}` never reach disk.
`gray mcp allow <name>` and `/mcp allow <name>` grant the same consent
from the command line or inside a session.

## `/mcp` in a session

```text
/mcp                  # every configured server and its state:
                      #   connecting / awaiting consent / ready (N tools)
                      #   / failed: … / denied / disabled
/mcp reload           # re-read both config files
/mcp allow <name>     # consent + start a project server
/mcp tools            # the flattened tool table
/mcp doctor [--deep] [name]  # pre-flight checks (below)
```

## `gray mcp` on the command line

```sh
gray mcp list [--json]                  # servers, source, consent state, warnings
gray mcp add <name> -- <cmd> [args…]    # stdio server into ~/.gray/mcp.json
gray mcp add <name> --url <https://…> [--header K=V]…
gray mcp add <name> --project -- <cmd>  # write ./.mcp.json instead
gray mcp add … --env K=V --timeout 60   # stdio env vars, per-call timeout
gray mcp remove <name> [--project]
gray mcp allow <name>                   # consent for a project server
gray mcp tools                          # connect to allowed servers, list tools
gray mcp doctor [--deep] [name]         # static config checks; --deep launches
                                        #   each stdio server and lists tools
gray mcp serve                          # stdio MCP server (below)
```

## Doctor

`gray mcp doctor` (also `/mcp doctor` in a session, and the `mcp_doctor`
tool — `{name?, deep?}` — published alongside the MCP tools) static-checks
every configured server *before* connecting: stdio command on PATH, URL
parses as http(s), `~`/absolute arg paths exist, and no hardcoded secrets
in `env`/`headers` (`${VAR}` indirection is the right shape). One
`ok | warn | fail` row per server. `--deep` then launches each stdio server
and lists its tools (30s each, sequential), flagging suspiciously-named
tools (`exec`, `shell`, `eval`, `rm`, `delete`, `write_remote`).
Report only — it never edits the config.

## Tool naming

`mcp__<server>__<tool>`: each half is sanitised to `[A-Za-z0-9_]`, the
whole name capped at 64 chars, collisions suffixed `_2`, `_3`, … The
model sees these as ordinary tools; call failures come back as `is_error`
results, and a server that drops is reconnected with backoff.

## `gray mcp serve` — gray as an MCP server

A stdio MCP server for clients that speak MCP (Claude Desktop, Cursor,
another gray). Three tools:

- **`gray_prompt`** — runs `gray -p <prompt> --json` and returns the
  final answer ending in `[session_id: …]` (structured content also
  carries `session_id`, `text`, `usage`). Pass `session_id` to continue a
  conversation — calls on the same id are serialised, so they queue
  rather than hit a locked session — and `cwd` to choose the working
  directory. A run is killed at 600 s, and emits progress notifications
  every 10 s when the client sent a progress token. The agent binary is
  `gray` on `PATH`, or `$GRAY_BIN`.
- **`gray_sessions`** — recent sessions, newest first: id, model, working
  directory, start time. `limit` (default 20).
- **`gray_session_read`** — one transcript as `role: text` lines.
  `session_id`, `last` (default 20 messages).

Claude Desktop / Cursor-style config:

```json
{
  "mcpServers": {
    "gray": {"command": "gray", "args": ["mcp", "serve"]}
  }
}
```

## Limitations

Non-goals for v1:

- The server exposes the agent surface only — `gray_prompt` runs whole
  tasks; gray's raw tools (`bash`, `read`, …) are not re-exported over
  MCP (that needs a `tool/run` host method that does not exist yet).
- `gray mcp serve` is stdio only; no streamable-HTTP server transport.
- The client speaks stdio and streamable HTTP only: no legacy SSE, no
  OAuth (static `headers` with `${VAR}` expansion instead).
- MCP tools only: no prompts, resources, sampling or elicitation.

## License

MIT.

---

Part of the [gray](https://github.com/vstaln/gray) plugin ecosystem —
the open-source AI agent harness. <https://gray.alignment.id>
