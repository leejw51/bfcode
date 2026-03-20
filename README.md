<p align="center">
  <img src="app.png" alt="bfcode" width="200">
</p>

<h1 align="center">bfcode</h1>

A terminal-based AI coding assistant powered by Grok. Think Claude Code / opencode, but backed by the Grok API.

## Features

- **Tool use**: read, write, edit files, run shell commands, glob, grep
- **Session management**: persistent conversations per project
- **Permission system**: prompts before destructive operations (bash, write, edit)
- **Project instructions**: auto-loads `AGENTS.md`, `CLAUDE.md`, or `BFCODE.md`
- **Plans**: save and load markdown plans as context
- **Retry with backoff**: automatic retries on rate limits and server errors

## Requirements

- Rust (edition 2024)
- A [Grok API key](https://x.ai/)

## Setup

```bash
cp .env.example .env
# Edit .env and add your API key
```

Export the key in your shell:

```bash
export GROK_API_KEY=your-api-key-here
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

### Commands

| Command | Description |
|---|---|
| `/help` | Show help |
| `/clear` | Clear current session |
| `/compact` | Compact conversation history |
| `/new` | Start a new session |
| `/sessions` | List all sessions |
| `/resume <id>` | Switch to a session |
| `/model <name>` | Change model |
| `/plan <name>` | Save a plan |
| `/plans` | List saved plans |
| `/quit` | Exit |

## Project Structure

```
cli/
├── src/
│   ├── main.rs          # CLI entry point, agent loop
│   ├── api.rs           # Grok API client with retry logic
│   ├── tools.rs         # Tool definitions and execution
│   ├── types.rs         # Request/response types, config
│   └── persistence.rs   # Session, config, and plan storage
├── Cargo.toml
└── Makefile
```

## License

MIT
