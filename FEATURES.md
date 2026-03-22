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

## Tools (29 built-in + dynamic)

### Core Tools (always available)

| Tool | Description |
|------|-------------|
| `read` | Read file contents with line numbers, offset/limit support |
| `write` | Create or overwrite files (auto-creates parent dirs) |
| `edit` | Replace exact string matches in files |
| `multiedit` | Multiple edits in a single tool call |
| `apply_patch` | Apply unified diff patches to one or more files |
| `bash` | Execute shell commands with configurable timeout |
| `glob` | Find files by glob pattern |
| `grep` | Search file contents with regex |
| `list_files` | List directory contents with sizes |
| `webfetch` | Fetch content from URLs |
| `browser_navigate` | Navigate browser to URL |
| `browser_screenshot` | Capture browser screenshot |
| `browser_click` | Click element in browser |
| `browser_type` | Type text in browser |
| `browser_evaluate` | Run JavaScript in browser |
| `browser_close` | Close browser session |
| `memory_save` | Save a memory entry |
| `memory_delete` | Delete a memory entry |
| `memory_list` | List all memory entries |
| `memory_search` | Search memory by keyword |
| `pdf_read` | Extract text from PDF files |
| `tts` | Text-to-speech audio generation |
| `batch` | Execute multiple tool calls in parallel |
| `task` | Spawn async subtasks |
| `todowrite` | Write todo/task items |
| `todoread` | Read todo/task items |
| `plan_enter` | Enter plan mode |
| `plan_exit` | Exit plan mode |
| `lsp` | Code intelligence (go-to-definition, references, hover, symbols) |

### Conditional Tools (API key required)

| Tool | Requires | Description |
|------|----------|-------------|
| `websearch` | `BRAVE_API_KEY` or `TAVILY_API_KEY` | Web search |
| `image_generate` | `OPENAI_API_KEY` | DALL-E image generation |

### Dynamic Tools (runtime)

- **MCP tools** — Discovered from connected MCP servers via `tools/list`
- **Plugin tools** — Namespaced as `plugin_{name}_{tool}`, defined in plugin manifests

### Agent Modes

| Mode | Tool Access |
|------|-------------|
| `Build` | All tools |
| `Plan` | Read/search only + plan file writes |
| `Explore` | Read/search only |

## LSP Integration

- Language server protocol client over JSON-RPC stdio
- Lazily starts and reuses servers per (language, project-root) pair
- Supported servers: **rust-analyzer** (`.rs`), **gopls** (`.go`), **typescript-language-server** (`.ts/.tsx/.js/.jsx`)
- Operations: `goToDefinition`, `findReferences`, `hover`, `documentSymbol`, `workspaceSymbol`

## MCP Support

- Model Context Protocol client for extensible tool discovery
- **Local transport** — stdio subprocess with custom env vars
- **Remote transport** — HTTP/SSE with custom headers
- Configuration in `mcp_servers` section of `config.json`
- Tools discovered via `tools/list` and executed via `tools/call`
- CLI: `bfcode mcp list`, `bfcode mcp tools`

## Plugin System

- Plugins are directories under `.bfcode/plugins/` or `~/.bfcode/plugins/`
- Each plugin has a `plugin.json` manifest declaring: name, version, description, entry point, tools, hooks
- Plugin tools namespaced as `plugin_{plugin_name}_{tool_name}` and executed via entry executable
- Plugin hooks merged into the hook manager at startup
- Scaffold new plugins: `bfcode plugin init <name>`
- CLI: `bfcode plugin list`, `bfcode plugin hooks`

## Lifecycle Hooks

- 9 hook types: `tool_before`, `tool_after`, `message_before`, `message_after`, `session_start`, `session_end`, `prompt_submit`, `response_complete`, `error`
- Hooks are shell commands (`sh -c`) with configurable timeout (default 10s)
- Context passed via env vars: `BFCODE_HOOK_TYPE`, `BFCODE_SESSION_ID`, `BFCODE_TOOL_NAME`, `BFCODE_TOOL_ARGS`, `BFCODE_TOOL_RESULT`, `BFCODE_MESSAGE`, `BFCODE_MODEL`, `BFCODE_ERROR`, `BFCODE_WORKING_DIR`
- `tool_before` hooks can output `BLOCK` or `DENY` to veto tool execution
- Glob-pattern matching for tool hooks (e.g., match `bash` or `write:*`)
- Loaded from both project and global config

## Telegram Bot

- Run bfcode as a Telegram bot: `bfcode telegram` (requires `TELEGRAM_BOT_TOKEN`)
- Long polling against the Telegram Bot API (no external bot framework)
- Per-chat isolated sessions with full LLM pipeline (tools, plugins, MCP, hooks)
- Access control via `TELEGRAM_ALLOWED_CHATS` (comma-separated chat IDs)
- Bot commands: `/start`, `/help`, `/model`, `/clear`
- Auto-splits long responses into 4000-char chunks

## Model Fallback Chain

- `FallbackChain` wraps multiple `ChatClient` instances (primary + fallbacks)
- Error classification: `RateLimit` (429), `Overloaded` (503), `Timeout`, `Auth` (401/403), `Unknown`
- Cooldown tracking with exponential backoff (30s min, 5min max, 2x factor)
- Context-overflow errors do not trigger failover
- Configured via `fallback_models` in config

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
| `/skill <name>` | Activate a skill |
| `/quit` | Exit |

## CLI Commands

```
bfcode                          # Start interactive chat (default)
bfcode chat [message...]        # Chat with optional initial message
bfcode session list             # List sessions
bfcode session new              # New session
bfcode session resume <id>      # Resume session
bfcode session export [id]      # Export as markdown
bfcode session fork [id] [-m N] # Fork session at message index
bfcode session children [id]    # List child sessions of a fork
bfcode model [name]             # Show/set model
bfcode clear                    # Clear session
bfcode compact                  # Compact conversation
bfcode plan list|create         # Manage plans
bfcode context env|summary|save|list|show  # Context management
bfcode memory list|show|save|delete        # Memory management
bfcode skills list|show|import  # Skill management
bfcode config                   # Show configuration
bfcode cfg show|validate|init|sources|migrate  # Advanced config management
bfcode undo [n]                 # Undo file changes
bfcode mcp list|tools           # MCP server management
bfcode plugin list|hooks|init   # Plugin management
bfcode gateway start|status|chat  # HTTP API gateway
bfcode daemon start|stop|status|install|uninstall|update  # Background service
bfcode cron list|add|remove|enable|disable  # Cron job scheduling
bfcode telegram                 # Start Telegram bot
bfcode doctor                   # Health checks (12 categories)
bfcode diagnostics              # System info for bug reports
```

## Session Management

- JSON file-based persistence (`.bfcode/sessions/`)
- Multiple concurrent sessions with switch/resume
- **Session forking** — fork a session at any message index, creating a child with `parent_id`
- List child sessions of any parent
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

## Health Checks

- `bfcode doctor` runs 12 checks: config, api_keys, api_connectivity, git, tools, chrome, tts, disk_space, sessions, memories, skills, lsp (+ async network)
- `bfcode diagnostics` collects OS, arch, Rust version, model/provider, session/memory/skill/cron counts, disk usage, API key status
- Each check returns Pass/Warn/Fail with message and optional details

## API Features

- Retry with exponential backoff (3 retries: 2s, 4s, 8s)
- 429 rate limit and 5xx server error handling
- 300-second request timeout
- Max 25 tool execution rounds per user message
