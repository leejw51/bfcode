<p align="center">
  <img src="app.png" alt="bfcode" width="200">
</p>

<h1 align="center">bfcode</h1>

A terminal-based AI coding assistant with multi-provider support. Think Claude Code / opencode, with support for Grok, OpenAI, and Anthropic APIs.

## Features

- **Multi-provider**: Grok, OpenAI, and Anthropic API support
- **TUI**: full-screen terminal interface with scrollable chat, input history, and status bar
- **Tool use**: read, write, edit files, run shell commands, glob, grep, apply patches, web fetch, PDF reading, and more
- **Browser automation**: headless Chrome/Chromium via CDP for navigation, screenshots, clicks, and JS evaluation
- **Web search**: search the web using Brave or Tavily APIs
- **Session management**: persistent conversations per project with export to markdown
- **Context memory**: markdown-based memory system with TF-IDF search
- **Permission system**: prompts before destructive operations (bash, write, edit)
- **Project instructions**: auto-loads `AGENTS.md`, `CLAUDE.md`, or `BFCODE.md`
- **Plans**: save and load markdown plans as context
- **Skills**: reusable prompt templates with YAML frontmatter
- **Undo**: revert file changes with per-file snapshots
- **Gateway server**: HTTP API for multi-user access with optional Tailscale integration
- **Daemon mode**: background service with auto-respawn and systemd/launchd install
- **Cron jobs**: schedule recurring tasks with persistent job storage
- **Hooks/plugins**: lifecycle hooks for tool, message, and session events
- **Doctor**: system diagnostics and health checks
- **Image handling**: clipboard and file path support
- **Cost tracking**: per-token pricing for OpenAI and Anthropic models
- **Retry with backoff**: automatic retries on rate limits and server errors

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
│   ├── main.rs          # CLI entry point, agent loop
│   ├── api.rs           # Multi-provider API client with retry logic
│   ├── tools.rs         # Tool definitions and execution
│   ├── types.rs         # Request/response types
│   ├── tui.rs           # Terminal user interface
│   ├── browser.rs       # Headless Chrome automation (CDP)
│   ├── config.rs        # Multi-source configuration
│   ├── context.rs       # Context and memory management
│   ├── cron.rs          # Cron job scheduler
│   ├── daemon.rs        # Background daemon service
│   ├── doctor.rs        # System diagnostics
│   ├── gateway.rs       # HTTP gateway server
│   ├── persistence.rs   # Session, config, and plan storage
│   ├── plugin.rs        # Lifecycle hooks system
│   ├── search.rs        # TF-IDF search engine
│   └── skill.rs         # Skill template system
├── Cargo.toml
└── Makefile
```

## License

MIT
