<p align="center">
  <img src="app.png" alt="bfcode" width="200">
</p>

<h1 align="center">bfcode</h1>

An autonomous AI coding agent for the terminal. Reads, writes, and edits code across multi-round tool loops — with Grok, OpenAI, and Anthropic.

## How It Works

bfcode runs an autonomous agent loop: you describe a task, and it reads files, writes code, runs commands, searches the web, and iterates — up to 25 rounds — without waiting for input. It spawns subagents for parallel subtasks, falls back to alternate models on failure, and auto-compacts context to stay within limits.

## Features

- **Autonomous agent loop** — 25-round tool execution, subagents (explore/plan/build), batch parallel calls, model fallback chain, context window guard, one-shot mode
- **Multi-provider** — Grok, OpenAI, Anthropic with streaming, retry/backoff, cost tracking
- **31 tools** — file read/write/edit/patch, bash, glob, grep, browser automation, web search, PDF, image generation, TTS, memory, tasks, plans
- **TUI** — full-screen terminal interface with scrollable chat, input history, code highlighting, undo
- **Custom agents** — define agents via `.bfcode/agents/*.md` with model, tools, and system prompt
- **Skills** — reusable prompt templates with YAML frontmatter
- **Sessions** — persistent conversations per project with export to markdown
- **Infrastructure** — HTTP gateway, background daemon, cron scheduler, lifecycle hooks, diagnostics

## Quick Start

```bash
export GROK_API_KEY=your-key      # or OPENAI_API_KEY or ANTHROPIC_API_KEY

cd cli
make install                       # builds and installs to ~/.local/bin

bfcode                             # start interactive session
bfcode chat "fix the failing test" # one-liner
```

## Commands

| Command | Description |
|---|---|
| `/help` | Show help |
| `/model [name]` | Show or change model |
| `/new` | Start a new session |
| `/sessions` | List sessions |
| `/resume <id>` | Switch session |
| `/compact` | Compact conversation |
| `/skill <name>` | Activate a skill |
| `/undo [count]` | Undo file changes |
| `/export [output]` | Export as markdown |
| `/quit` | Exit |

```bash
bfcode gateway start       # HTTP API server (multi-user, Tailscale support)
bfcode daemon start        # background service with auto-respawn
bfcode cron add 5m "task"  # scheduled recurring tasks
bfcode doctor              # system diagnostics
```

## Configuration

Priority: env vars > project `.bfcode/config.json` > global `~/.bfcode/config.json` > defaults. JSON and YAML supported.

## Project Structure

```
cli/src/
├── main.rs        # Agent loop (25 rounds)
├── api.rs         # Multi-provider API client
├── tools.rs       # 31 tool definitions
├── agent.rs       # Custom agents and subagent modes
├── fallback.rs    # Model fallback chain
├── guard.rs       # Context window guard
├── tui.rs         # Terminal UI
├── browser.rs     # Chrome automation (CDP)
├── context.rs     # Context and memory
├── config.rs      # Configuration
├── gateway.rs     # HTTP gateway
├── daemon.rs      # Background daemon
├── cron.rs        # Cron scheduler
├── plugin.rs      # Lifecycle hooks
├── skill.rs       # Skill templates
├── search.rs      # TF-IDF search
├── persistence.rs # Session storage
├── doctor.rs      # Diagnostics
└── types.rs       # Types and models
```

## License

MIT
