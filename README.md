<p align="center">
  <img src="app.png" alt="bfcode" width="200">
</p>

<h1 align="center">bfcode</h1>

A terminal-based AI coding assistant with multi-provider support. Think Claude Code / opencode, with support for Grok, OpenAI, and Anthropic APIs.

## Features

### Autonomous Agent
- **25-round agent loop**: AI autonomously calls tools, reads results, and iterates up to 25 rounds without human input
- **Subagents**: spawns isolated child agents (explore/plan/build) for complex subtasks with restricted tool sets
- **Custom agents**: define agents via markdown files in `.bfcode/agents/` with model, tools, and system prompt
- **Batch execution**: runs up to 25 tool calls in parallel for maximum throughput
- **Model fallback chain**: auto-switches to backup model on rate limit, outage, or timeout with cooldown tracking
- **Context window guard**: pre-flight checks prevent context overflow; hard floor at 16K tokens, preemptive compaction at 90%
- **One-shot mode**: fully autonomous execution with auto-approved permissions (`--oneshot`)

### Multi-Provider LLM
- **Grok, OpenAI, Anthropic**: seamless provider switching with automatic detection from model name
- **Streaming**: real-time SSE streaming with animated status indicators
- **Retry with backoff**: automatic retries on rate limits and server errors
- **Cost tracking**: per-token pricing for all supported models

### Tools & Capabilities
- **File operations**: read, write, edit, multi-edit, apply patches, glob, grep, list files
- **Shell execution**: run bash commands with permission gates
- **Browser automation**: headless Chrome/Chromium via CDP for navigation, screenshots, clicks, and JS evaluation
- **Web search**: search the web using Brave or Tavily APIs
- **PDF reading**: extract and analyze PDF content
- **Image generation**: DALL-E integration for image creation
- **Text-to-speech**: TTS synthesis

### TUI & Interface
- **Full-screen TUI**: scrollable chat, input history, status bar, code block highlighting
- **Session management**: persistent conversations per project with export to markdown
- **Context memory**: markdown-based memory system with TF-IDF search
- **Permission system**: prompts before destructive operations (bash, write, edit)
- **Project instructions**: auto-loads `AGENTS.md`, `CLAUDE.md`, or `BFCODE.md`
- **Plans**: save and load markdown plans as context
- **Skills**: reusable prompt templates with YAML frontmatter
- **Undo**: revert file changes with per-file snapshots
- **Image handling**: clipboard and file path support

### Infrastructure
- **Gateway server**: HTTP API for multi-user access with optional Tailscale integration
- **Daemon mode**: background service with auto-respawn and systemd/launchd install
- **Cron jobs**: schedule recurring tasks with persistent job storage
- **Hooks/plugins**: lifecycle hooks for tool, message, and session events
- **Doctor**: system diagnostics and health checks

## Requirements

- Rust (edition 2024)
- An API key from one of the supported providers

## Setup

Set your API key as an environment variable:

```bash
# Grok (default)
export GROK_API_KEY=your-key

# OpenAI
export OPENAI_API_KEY=your-key

# Anthropic
export ANTHROPIC_API_KEY=your-key
```

## Build & Install

```bash
cd cli

# Debug build
make build

# Release build + install to ~/.local/bin
make install
```

## Usage

```bash
bfcode
```

### Interactive Commands

| Command | Description |
|---|---|
| `/help` | Show help |
| `/clear` | Clear current session |
| `/compact` | Compact conversation history |
| `/new` | Start a new session |
| `/sessions` | List all sessions |
| `/resume <id>` | Switch to a session |
| `/model [name]` | Show or change model |
| `/plan <name>` | Save a plan |
| `/plans` | List saved plans |
| `/skills` | List available skills |
| `/skill <name>` | Activate a skill |
| `/undo [count]` | Undo last file changes |
| `/export [output]` | Export session as markdown |
| `/context` | Show compaction summary |
| `/cron` | Manage cron jobs |
| `/quit` | Exit |

### CLI Subcommands

```bash
bfcode gateway start [--listen <addr>] [--tailscale]   # Start HTTP API server
bfcode gateway status [--url <url>]                     # Check gateway status
bfcode gateway chat --url <url> <message>               # Remote chat

bfcode daemon start|stop|status|install|uninstall       # Manage background daemon
bfcode cron list|add|remove|enable|disable              # Manage cron jobs
bfcode doctor                                           # Run system diagnostics
bfcode config                                           # Show current config
bfcode session list|new|resume|export                   # Manage sessions
bfcode memory list|show|save|delete                     # Manage context memory
bfcode skills list|show|import                          # Manage skills
```

## Configuration

Config files are loaded with priority: env vars > project (`.bfcode/config.json`) > global (`~/.bfcode/config.json`) > defaults. Both JSON and YAML formats are supported.

```bash
bfcode cfg init [--format json|yaml] [--project]   # Initialize config
bfcode cfg show                                     # Show merged config
bfcode cfg validate                                 # Validate config files
bfcode cfg sources                                  # Show config file locations
```

## Project Structure

```
cli/
├── src/
│   ├── main.rs          # CLI entry point, 25-round agent loop
│   ├── api.rs           # Multi-provider API client with retry logic
│   ├── tools.rs         # Tool definitions and execution (31 tools)
│   ├── types.rs         # Request/response types
│   ├── tui.rs           # Terminal user interface
│   ├── agent.rs         # Custom agent definitions and subagent modes
│   ├── browser.rs       # Headless Chrome automation (CDP)
│   ├── config.rs        # Multi-source configuration
│   ├── context.rs       # Context and memory management
│   ├── cron.rs          # Cron job scheduler
│   ├── daemon.rs        # Background daemon service
│   ├── doctor.rs        # System diagnostics
│   ├── fallback.rs      # Model fallback chain with cooldown tracking
│   ├── gateway.rs       # HTTP gateway server
│   ├── guard.rs         # Context window guard (overflow protection)
│   ├── persistence.rs   # Session, config, and plan storage
│   ├── plugin.rs        # Lifecycle hooks system
│   ├── search.rs        # TF-IDF search engine
│   └── skill.rs         # Skill template system
├── Cargo.toml
└── Makefile
```

## License

MIT
