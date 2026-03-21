# bfcode Features

## Multi-Provider AI Support

- **Grok (X.AI)** — Default provider, OpenAI-compatible API
- **OpenAI** — GPT-4o, o1, o3, o4 models
- **Anthropic** — Claude models with native API format
- Auto-detection from model name: `grok-*` → Grok, `gpt-*`/`o1-*`/`o3-*`/`o4-*` → OpenAI, `claude-*` → Anthropic
- Per-provider API keys: `GROK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`
- Switch models: `bfcode model claude-sonnet-4-20250514` or `/model gpt-4o`

## Streaming Responses

- Real-time token-by-token output as the model generates
- SSE (Server-Sent Events) parsing for both OpenAI-compatible and Anthropic formats
- Streaming tool call accumulation (function name and arguments arrive incrementally)
- Automatic fallback to non-streaming for mock/test clients

## Tools (8 built-in)

| Tool | Description |
|------|-------------|
| `read` | Read file contents with line numbers, offset/limit support |
| `write` | Create or overwrite files (auto-creates parent dirs) |
| `edit` | Replace exact string matches in files |
| `apply_patch` | Apply unified diff patches to one or more files |
| `bash` | Execute shell commands with configurable timeout |
| `glob` | Find files by glob pattern |
| `grep` | Search file contents with regex |
| `list_files` | List directory contents with sizes |

## Undo/Revert System

- Automatic file snapshots before every `write`, `edit`, and `apply_patch`
- `/undo [n]` — Revert last N file changes in interactive mode
- `bfcode undo [n]` — CLI undo command
- Snapshots stored in `.bfcode/snapshots/{session_id}/`

## Token-Aware Compaction

- Token estimation (~4 chars/token, like opencode)
- Auto-compact when conversation exceeds 80% of model context limit
- Model-specific context limits: Grok 131K, GPT-4o 128K, Claude 200K
- Smart pruning: truncates old tool outputs first, then summarizes
- Structured compaction summary with Goal/Discoveries/Accomplished/Files sections

## Interactive REPL

### Slash Commands

| Command | Description |
|---------|-------------|
| `/help` | Show command help |
| `/clear` | Clear session conversation |
| `/compact` | Manually compact conversation |
| `/new` | Create new session |
| `/sessions` | List all sessions |
| `/resume <id>` | Switch to session by ID |
| `/model [name]` | Show/change model (auto-detects provider) |
| `/plan <name>` | Create a plan |
| `/plans` | List saved plans |
| `/export [file]` | Export session as markdown |
| `/context` | Show compaction summary |
| `/undo [n]` | Undo last N file changes |
| `/quit` | Exit |

## CLI Commands

```
bfcode                          # Start interactive chat (default)
bfcode chat [message...]        # Chat with optional initial message
bfcode session list             # List sessions
bfcode session new              # New session
bfcode session resume <id>      # Resume session
bfcode session export [id]      # Export as markdown
bfcode model [name]             # Show/set model
bfcode clear                    # Clear session
bfcode compact                  # Compact conversation
bfcode plan list|create         # Manage plans
bfcode context env|summary|save|list|show  # Context management
bfcode config                   # Show configuration
bfcode undo [n]                 # Undo file changes
```

## Session Management

- JSON file-based persistence (`.bfcode/sessions/`)
- Multiple concurrent sessions with switch/resume
- Auto-title from first user message
- Token usage tracking per session
- Session export as readable markdown transcripts

## Context System

- **Project instructions** — Auto-loads `AGENTS.md`, `CLAUDE.md`, `BFCODE.md`, `.bfcode/instructions.md`
- **Plans** — Markdown files in `.bfcode/plans/`, loaded into system prompt
- **Environment snapshots** — Git status, project structure, platform info
- **Compaction summaries** — Structured markdown with Goal/Instructions/Discoveries/Accomplished/Files
- **Context auto-loading** — All `.bfcode/context/*.md` files injected into system prompt

## Permission System

- Interactive permission prompts for dangerous tools: `bash`, `write`, `edit`, `apply_patch`
- `y` (allow once), `a` (allow always for session), `n` (deny)
- Session-scoped wildcard permissions

## Output Safety

- Max 2000 lines / 50KB per tool output
- Max 2000 chars per line (truncated with ellipsis)
- Grep limited to 200 matches
- Glob limited to 200 files

## API Features

- Retry with exponential backoff (3 retries: 2s, 4s, 8s)
- 429 rate limit and 5xx server error handling
- 300-second request timeout
- Max 25 tool execution rounds per user message
