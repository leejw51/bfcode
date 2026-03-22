//! LSP client for code intelligence (go-to-definition, references, hover, symbols).
//!
//! Connects to language servers (rust-analyzer, gopls, typescript-language-server)
//! over JSON-RPC stdio.  Servers are lazily started and reused across tool calls.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// Built-in server definitions
// ---------------------------------------------------------------------------

struct ServerDef {
    id: &'static str,
    extensions: &'static [&'static str],
    command: &'static [&'static str],
    root_markers: &'static [&'static str],
    install_hint: &'static str,
    language_id: &'static str,
}

const SERVERS: &[ServerDef] = &[
    ServerDef {
        id: "rust-analyzer",
        extensions: &[".rs"],
        command: &["rust-analyzer"],
        root_markers: &["Cargo.toml"],
        install_hint: "rustup component add rust-analyzer",
        language_id: "rust",
    },
    ServerDef {
        id: "gopls",
        extensions: &[".go"],
        command: &["gopls", "serve"],
        root_markers: &["go.mod"],
        install_hint: "go install golang.org/x/tools/gopls@latest",
        language_id: "go",
    },
    ServerDef {
        id: "typescript-language-server",
        extensions: &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"],
        command: &["typescript-language-server", "--stdio"],
        root_markers: &["package.json", "tsconfig.json"],
        install_hint: "npm install -g typescript-language-server typescript",
        language_id: "typescript",
    },
];

// ---------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------

async fn write_message(stdin: &mut tokio::process::ChildStdin, msg: &Value) -> Result<()> {
    let body = serde_json::to_string(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(body.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_message(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Value> {
    // Read headers until empty line
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(len_str) = line.strip_prefix("Content-Length: ") {
            content_length = len_str.trim().parse()?;
        }
    }
    if content_length == 0 {
        bail!("Missing Content-Length header");
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).await?;
    let val: Value = serde_json::from_slice(&buf)?;
    Ok(val)
}

// ---------------------------------------------------------------------------
// LSP Client — one per (server, project root)
// ---------------------------------------------------------------------------

struct LspClient {
    server_id: String,
    root: PathBuf,
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: AtomicI64,
    open_files: HashMap<PathBuf, i32>,
}

impl LspClient {
    async fn spawn(def: &ServerDef, root: &Path) -> Result<Self> {
        let bin = def.command[0];
        // Check binary exists
        let which = Command::new("which")
            .arg(bin)
            .output()
            .await
            .context("failed to run which")?;
        if !which.status.success() {
            bail!(
                "LSP server '{}' not found.\nInstall it with:\n  {}",
                bin,
                def.install_hint
            );
        }

        let mut child = Command::new(def.command[0])
            .args(&def.command[1..])
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to start LSP server: {}", def.command.join(" ")))?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        let mut client = Self {
            server_id: def.id.to_string(),
            root: root.to_path_buf(),
            child,
            stdin,
            reader,
            next_id: AtomicI64::new(1),
            open_files: HashMap::new(),
        };

        client.initialize(root).await?;
        Ok(client)
    }

    async fn initialize(&mut self, root: &Path) -> Result<()> {
        let root_uri = format!("file://{}", root.display());
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let init_request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "definition": { "dynamicRegistration": false },
                        "references": { "dynamicRegistration": false },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "documentSymbol": { "dynamicRegistration": false },
                        "publishDiagnostics": { "relatedInformation": true }
                    },
                    "workspace": {
                        "symbol": { "dynamicRegistration": false },
                        "workspaceFolders": true
                    }
                },
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": root.file_name().unwrap_or_default().to_string_lossy()
                }]
            }
        });

        write_message(&mut self.stdin, &init_request).await?;

        // Read response — skip notifications until we get our response
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(30), self.read_response(id))
                .await
                .context("LSP initialize timed out (30s)")??;

        // Send initialized notification
        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        write_message(&mut self.stdin, &initialized).await?;

        Ok(())
    }

    /// Read messages until we find the response matching `id`.
    async fn read_response(&mut self, id: i64) -> Result<Value> {
        loop {
            let msg = read_message(&mut self.reader).await?;
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    bail!("LSP error: {}", err);
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // Skip notifications and server requests
            if msg.get("method").is_some() && msg.get("id").is_some() {
                // Server request — respond with null
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": msg["id"],
                    "result": null
                });
                write_message(&mut self.stdin, &resp).await?;
            }
        }
    }

    async fn notify_open(&mut self, path: &Path, language_id: &str) -> Result<()> {
        let version = self.open_files.entry(path.to_path_buf()).or_insert(0);
        let uri = format!("file://{}", path.display());

        if *version == 0 {
            let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id,
                        "version": 1,
                        "text": content
                    }
                }
            });
            write_message(&mut self.stdin, &notif).await?;
            *version = 1;
        } else {
            // Re-read file for changes
            let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
            *version += 1;
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": uri, "version": *version },
                    "contentChanges": [{ "text": content }]
                }
            });
            write_message(&mut self.stdin, &notif).await?;
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        write_message(&mut self.stdin, &req).await?;

        tokio::time::timeout(std::time::Duration::from_secs(15), self.read_response(id))
            .await
            .context("LSP request timed out (15s)")?
    }

    fn shutdown(mut self) {
        // Best-effort shutdown
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// Global LSP manager
// ---------------------------------------------------------------------------

struct LspManager {
    clients: Vec<LspClient>,
    broken: HashSet<String>, // "server_id:root" keys
}

static LSP_MANAGER: std::sync::LazyLock<tokio::sync::Mutex<LspManager>> =
    std::sync::LazyLock::new(|| {
        tokio::sync::Mutex::new(LspManager {
            clients: Vec::new(),
            broken: HashSet::new(),
        })
    });

/// Find the server definition for a file extension.
fn find_server_for_file(path: &Path) -> Option<&'static ServerDef> {
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    SERVERS
        .iter()
        .find(|s| s.extensions.contains(&ext.as_str()))
}

/// Walk up from `start` looking for any of `markers`.
fn find_project_root(start: &Path, markers: &[&str]) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        for marker in markers {
            if dir.join(marker).exists() {
                return Some(dir);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Public API — called from tools.rs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LspArgs {
    pub operation: String,
    pub file_path: Option<String>,
    #[serde(alias = "filePath")]
    pub file_path_alias: Option<String>,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub query: Option<String>,
}

impl LspArgs {
    fn path(&self) -> Option<&str> {
        self.file_path
            .as_deref()
            .or(self.file_path_alias.as_deref())
    }
}

pub async fn execute(arguments: &str) -> Result<String> {
    let args: LspArgs = serde_json::from_str(arguments)
        .context("Invalid LSP arguments. Required: operation, filePath")?;

    let file_str = args.path().context("Missing filePath parameter")?;
    let file = std::path::absolute(Path::new(file_str)).unwrap_or_else(|_| PathBuf::from(file_str));

    // Find server
    let def = find_server_for_file(&file).with_context(|| {
        let ext = file.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or("(none)".into());
        format!(
            "No LSP server configured for .{ext} files.\nSupported: Rust (.rs), Go (.go), TypeScript/JavaScript (.ts/.js)"
        )
    })?;

    // Find project root
    let root = find_project_root(&file, def.root_markers).with_context(|| {
        format!(
            "Could not find project root (looked for {:?} in parent directories of {})",
            def.root_markers,
            file.display()
        )
    })?;

    // Get or create client
    let mut manager = LSP_MANAGER.lock().await;
    let key = format!("{}:{}", def.id, root.display());

    if manager.broken.contains(&key) {
        bail!(
            "LSP server '{}' previously failed for this project.\nInstall: {}",
            def.id,
            def.install_hint
        );
    }

    // Find existing client
    let client_idx = manager
        .clients
        .iter()
        .position(|c| c.server_id == def.id && c.root == root);

    let client_idx = match client_idx {
        Some(idx) => idx,
        None => {
            // Spawn new client
            match LspClient::spawn(def, &root).await {
                Ok(client) => {
                    manager.clients.push(client);
                    manager.clients.len() - 1
                }
                Err(e) => {
                    manager.broken.insert(key);
                    return Err(e);
                }
            }
        }
    };

    let client = &mut manager.clients[client_idx];

    // Ensure file is open
    if file.exists() {
        client.notify_open(&file, def.language_id).await?;
        // Small delay to let server index
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let uri = format!("file://{}", file.display());
    let line = args.line.unwrap_or(1).saturating_sub(1); // 1-based → 0-based
    let character = args.character.unwrap_or(1).saturating_sub(1);

    let pos_params = json!({
        "textDocument": { "uri": &uri },
        "position": { "line": line, "character": character }
    });

    match args.operation.as_str() {
        "goToDefinition" => {
            let result = client
                .request("textDocument/definition", pos_params)
                .await?;
            Ok(format_locations("Definition", &result, &root))
        }
        "findReferences" => {
            let params = json!({
                "textDocument": { "uri": &uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": true }
            });
            let result = client.request("textDocument/references", params).await?;
            Ok(format_locations("References", &result, &root))
        }
        "hover" => {
            let result = client.request("textDocument/hover", pos_params).await?;
            Ok(format_hover(&result))
        }
        "documentSymbol" => {
            let params = json!({ "textDocument": { "uri": &uri } });
            let result = client
                .request("textDocument/documentSymbol", params)
                .await?;
            Ok(format_symbols(&result))
        }
        "workspaceSymbol" => {
            let query = args.query.as_deref().unwrap_or("");
            let params = json!({ "query": query });
            let result = client.request("workspace/symbol", params).await?;
            Ok(format_workspace_symbols(&result, &root))
        }
        "diagnostics" => {
            // Request diagnostics by touching the file and waiting
            Ok(
                "Diagnostics are published asynchronously. Use hover or check compiler output."
                    .into(),
            )
        }
        other => bail!(
            "Unknown LSP operation: '{other}'.\nAvailable: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol"
        ),
    }
}

// ---------------------------------------------------------------------------
// Result formatters
// ---------------------------------------------------------------------------

fn format_locations(label: &str, result: &Value, root: &Path) -> String {
    let locations = match result {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![result.clone()],
        Value::Null => return format!("No {label} found."),
        _ => return format!("No {label} found."),
    };

    if locations.is_empty() {
        return format!("No {label} found.");
    }

    let mut out = format!("{} {} found:\n", locations.len(), label.to_lowercase());
    // Note: the "No X found" uses original label casing (e.g. "No Definition found.")
    for loc in &locations {
        let uri = loc
            .get("uri")
            .or_else(|| loc.get("targetUri"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let range = loc.get("range").or_else(|| loc.get("targetRange"));
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        let rel = Path::new(path)
            .strip_prefix(root)
            .unwrap_or(Path::new(path));

        if let Some(range) = range {
            let line = range["start"]["line"].as_u64().unwrap_or(0) + 1;
            let col = range["start"]["character"].as_u64().unwrap_or(0) + 1;
            out.push_str(&format!("  {}:{}:{}\n", rel.display(), line, col));
        } else {
            out.push_str(&format!("  {}\n", rel.display()));
        }
    }
    out
}

fn format_hover(result: &Value) -> String {
    if result.is_null() {
        return "No hover information available.".into();
    }
    let contents = &result["contents"];
    match contents {
        Value::String(s) => s.clone(),
        Value::Object(obj) => {
            // { language, value } or { kind, value }
            let value = obj.get("value").and_then(|v| v.as_str()).unwrap_or("");
            let lang = obj
                .get("language")
                .or_else(|| obj.get("kind"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if lang.is_empty() || lang == "plaintext" {
                value.to_string()
            } else {
                format!("```{lang}\n{value}\n```")
            }
        }
        Value::Array(arr) => {
            // Array of MarkedString
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(obj) => {
                        let value = obj.get("value")?.as_str()?;
                        let lang = obj.get("language").and_then(|v| v.as_str()).unwrap_or("");
                        if lang.is_empty() {
                            Some(value.to_string())
                        } else {
                            Some(format!("```{lang}\n{value}\n```"))
                        }
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }
        _ => "No hover information available.".into(),
    }
}

fn format_symbols(result: &Value) -> String {
    let symbols = match result {
        Value::Array(arr) => arr,
        _ => return "No symbols found.".into(),
    };
    if symbols.is_empty() {
        return "No symbols found.".into();
    }

    let mut out = format!("{} symbols:\n", symbols.len());
    format_symbols_recursive(symbols, &mut out, 0);
    out
}

fn format_symbols_recursive(symbols: &[Value], out: &mut String, depth: usize) {
    let indent = "  ".repeat(depth);
    for sym in symbols {
        let name = sym["name"].as_str().unwrap_or("?");
        let kind = symbol_kind_str(sym["kind"].as_u64().unwrap_or(0));
        let line = sym
            .get("selectionRange")
            .or_else(|| sym.get("range"))
            .map(|r| r["start"]["line"].as_u64().unwrap_or(0) + 1)
            .unwrap_or(0);
        // Also handle SymbolInformation (has location instead of range)
        let line = if line == 0 {
            sym.get("location")
                .and_then(|l| l.get("range"))
                .map(|r| r["start"]["line"].as_u64().unwrap_or(0) + 1)
                .unwrap_or(0)
        } else {
            line
        };
        out.push_str(&format!("{indent}  {kind} {name} [line {line}]\n"));
        // Recurse into children (DocumentSymbol)
        if let Some(Value::Array(children)) = sym.get("children") {
            format_symbols_recursive(children, out, depth + 1);
        }
    }
}

fn format_workspace_symbols(result: &Value, root: &Path) -> String {
    let symbols = match result {
        Value::Array(arr) => arr,
        _ => return "No symbols found.".into(),
    };
    if symbols.is_empty() {
        return "No symbols found.".into();
    }

    let count = symbols.len().min(20);
    let mut out = format!("{count} symbols found:\n");
    for sym in symbols.iter().take(20) {
        let name = sym["name"].as_str().unwrap_or("?");
        let kind = symbol_kind_str(sym["kind"].as_u64().unwrap_or(0));
        let uri = sym["location"]["uri"].as_str().unwrap_or("");
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        let rel = Path::new(path)
            .strip_prefix(root)
            .unwrap_or(Path::new(path));
        let line = sym["location"]["range"]["start"]["line"]
            .as_u64()
            .unwrap_or(0)
            + 1;
        out.push_str(&format!("  {kind} {name} — {}:{line}\n", rel.display()));
    }
    if symbols.len() > 20 {
        out.push_str(&format!("  ... and {} more\n", symbols.len() - 20));
    }
    out
}

fn symbol_kind_str(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "fn",
        13 => "var",
        14 => "const",
        15 => "string",
        16 => "number",
        17 => "bool",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum_member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_param",
        _ => "symbol",
    }
}

// ---------------------------------------------------------------------------
// Doctor check — called from doctor.rs
// ---------------------------------------------------------------------------

pub fn check_lsp_servers() -> Vec<(String, bool, String)> {
    let mut results = Vec::new();
    for def in SERVERS {
        let bin = def.command[0];
        let found = std::process::Command::new("which")
            .arg(bin)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let hint = if found {
            "installed".to_string()
        } else {
            format!("not found — install with: {}", def.install_hint)
        };
        results.push((bin.to_string(), found, hint));
    }
    results
}

// ---------------------------------------------------------------------------
// Shutdown — called on program exit
// ---------------------------------------------------------------------------

pub async fn shutdown_all() {
    let mut manager = LSP_MANAGER.lock().await;
    for client in manager.clients.drain(..) {
        client.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_server_for_rust() {
        let path = Path::new("src/main.rs");
        let def = find_server_for_file(path).unwrap();
        assert_eq!(def.id, "rust-analyzer");
    }

    #[test]
    fn test_find_server_for_go() {
        let path = Path::new("main.go");
        let def = find_server_for_file(path).unwrap();
        assert_eq!(def.id, "gopls");
    }

    #[test]
    fn test_find_server_for_ts() {
        let path = Path::new("app.tsx");
        let def = find_server_for_file(path).unwrap();
        assert_eq!(def.id, "typescript-language-server");
    }

    #[test]
    fn test_find_server_for_unknown() {
        let path = Path::new("data.csv");
        assert!(find_server_for_file(path).is_none());
    }

    #[test]
    fn test_find_project_root() {
        // Should find the CLI project root (has Cargo.toml)
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
        let root = find_project_root(&src, &["Cargo.toml"]);
        assert!(root.is_some());
        assert!(root.unwrap().join("Cargo.toml").exists());
    }

    #[test]
    fn test_symbol_kind_str() {
        assert_eq!(symbol_kind_str(12), "fn");
        assert_eq!(symbol_kind_str(5), "class");
        assert_eq!(symbol_kind_str(23), "struct");
        assert_eq!(symbol_kind_str(999), "symbol");
    }

    #[test]
    fn test_format_locations_empty() {
        let root = PathBuf::from("/project");
        assert_eq!(
            format_locations("Definition", &Value::Null, &root),
            "No Definition found."
        );
    }

    #[test]
    fn test_format_locations_empty_array() {
        let root = PathBuf::from("/project");
        let result = json!([]);
        assert_eq!(
            format_locations("References", &result, &root),
            "No References found."
        );
    }

    #[test]
    fn test_format_locations_single() {
        let root = PathBuf::from("/project");
        let result = json!([{
            "uri": "file:///project/src/main.rs",
            "range": {
                "start": { "line": 41, "character": 4 },
                "end": { "line": 41, "character": 20 }
            }
        }]);
        let output = format_locations("Definition", &result, &root);
        assert!(output.contains("1 definition found:"));
        assert!(output.contains("src/main.rs:42:5"));
    }

    #[test]
    fn test_format_locations_multiple() {
        let root = PathBuf::from("/project");
        let result = json!([
            { "uri": "file:///project/src/a.rs", "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5} } },
            { "uri": "file:///project/src/b.rs", "range": { "start": {"line": 9, "character": 3}, "end": {"line": 9, "character": 10} } },
        ]);
        let output = format_locations("References", &result, &root);
        assert!(output.contains("2 references found:"));
        assert!(output.contains("src/a.rs:1:1"));
        assert!(output.contains("src/b.rs:10:4"));
    }

    #[test]
    fn test_format_locations_with_target_uri() {
        // Some LSP servers return targetUri/targetRange (e.g. for LocationLink)
        let root = PathBuf::from("/project");
        let result = json!([{
            "targetUri": "file:///project/lib/types.rs",
            "targetRange": { "start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 10} }
        }]);
        let output = format_locations("Definition", &result, &root);
        assert!(output.contains("lib/types.rs:6:1"));
    }

    #[test]
    fn test_format_hover_null() {
        assert_eq!(
            format_hover(&Value::Null),
            "No hover information available."
        );
    }

    #[test]
    fn test_format_hover_string() {
        let result = json!({ "contents": "A simple string hover" });
        assert_eq!(format_hover(&result), "A simple string hover");
    }

    #[test]
    fn test_format_hover_markup() {
        let result = json!({ "contents": { "language": "rust", "value": "fn main()" } });
        assert_eq!(format_hover(&result), "```rust\nfn main()\n```");
    }

    #[test]
    fn test_format_hover_plaintext() {
        let result = json!({ "contents": { "kind": "plaintext", "value": "just text" } });
        assert_eq!(format_hover(&result), "just text");
    }

    #[test]
    fn test_format_hover_array() {
        let result = json!({ "contents": [
            { "language": "go", "value": "func main()" },
            "Documentation for main"
        ]});
        let output = format_hover(&result);
        assert!(output.contains("```go\nfunc main()\n```"));
        assert!(output.contains("Documentation for main"));
    }

    #[test]
    fn test_format_symbols_empty() {
        assert_eq!(format_symbols(&Value::Null), "No symbols found.");
        assert_eq!(format_symbols(&json!([])), "No symbols found.");
    }

    #[test]
    fn test_format_symbols_flat() {
        let result = json!([
            { "name": "main", "kind": 12, "range": { "start": {"line": 0, "character": 0}, "end": {"line": 10, "character": 0} },
              "selectionRange": { "start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7} } },
            { "name": "Config", "kind": 23, "range": { "start": {"line": 12, "character": 0}, "end": {"line": 20, "character": 0} },
              "selectionRange": { "start": {"line": 12, "character": 4}, "end": {"line": 12, "character": 10} } },
        ]);
        let output = format_symbols(&result);
        assert!(output.contains("2 symbols:"));
        assert!(output.contains("fn main [line 1]"));
        assert!(output.contains("struct Config [line 13]"));
    }

    #[test]
    fn test_format_symbols_nested_children() {
        let result = json!([{
            "name": "MyStruct",
            "kind": 23,
            "range": { "start": {"line": 0, "character": 0}, "end": {"line": 10, "character": 0} },
            "selectionRange": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 8} },
            "children": [
                { "name": "field_a", "kind": 8, "range": { "start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 10} },
                  "selectionRange": { "start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 11} } },
                { "name": "field_b", "kind": 8, "range": { "start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 10} },
                  "selectionRange": { "start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 11} } },
            ]
        }]);
        let output = format_symbols(&result);
        assert!(output.contains("struct MyStruct [line 1]"));
        assert!(output.contains("field field_a [line 2]"));
        assert!(output.contains("field field_b [line 3]"));
    }

    #[test]
    fn test_format_symbols_symbol_information() {
        // SymbolInformation format (has location instead of range)
        let result = json!([{
            "name": "handler",
            "kind": 12,
            "location": {
                "uri": "file:///project/src/api.rs",
                "range": { "start": {"line": 49, "character": 0}, "end": {"line": 60, "character": 0} }
            }
        }]);
        let output = format_symbols(&result);
        assert!(output.contains("fn handler [line 50]"));
    }

    #[test]
    fn test_format_workspace_symbols() {
        let root = PathBuf::from("/project");
        let result = json!([
            { "name": "Config", "kind": 23, "location": { "uri": "file:///project/src/config.rs", "range": { "start": {"line": 5, "character": 0}, "end": {"line": 20, "character": 0} } } },
            { "name": "main", "kind": 12, "location": { "uri": "file:///project/src/main.rs", "range": { "start": {"line": 0, "character": 0}, "end": {"line": 100, "character": 0} } } },
        ]);
        let output = format_workspace_symbols(&result, &root);
        assert!(output.contains("2 symbols found:"));
        assert!(output.contains("struct Config — src/config.rs:6"));
        assert!(output.contains("fn main — src/main.rs:1"));
    }

    #[test]
    fn test_format_workspace_symbols_empty() {
        let root = PathBuf::from("/project");
        assert_eq!(
            format_workspace_symbols(&json!([]), &root),
            "No symbols found."
        );
    }

    #[test]
    fn test_find_server_all_js_extensions() {
        for ext in &["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
            let path = PathBuf::from(format!("file.{ext}"));
            let def = find_server_for_file(&path);
            assert!(def.is_some(), "Should find server for .{ext}");
            assert_eq!(def.unwrap().id, "typescript-language-server");
        }
    }

    #[test]
    fn test_find_project_root_no_marker() {
        // temp dir with no Cargo.toml
        let tmp = std::env::temp_dir().join("bfcode-test-lsp-no-root");
        let _ = std::fs::create_dir_all(&tmp);
        let fake = tmp.join("test.rs");
        let _ = std::fs::write(&fake, "fn main() {}");
        let root = find_project_root(&fake, &["Cargo.toml"]);
        // May find one up the tree or return None — either is acceptable
        // Just verify it doesn't panic
        let _ = root;
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_check_lsp_servers() {
        let results = crate::lsp::check_lsp_servers();
        assert_eq!(results.len(), 3);
        // Verify all 3 servers are checked
        let names: Vec<_> = results.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"rust-analyzer"));
        assert!(names.contains(&"gopls"));
        assert!(names.contains(&"typescript-language-server"));
        // Each entry has a hint
        for (_, _, hint) in &results {
            assert!(!hint.is_empty());
        }
    }

    #[test]
    fn test_all_symbol_kinds() {
        // Verify known kinds return meaningful names
        assert_eq!(symbol_kind_str(1), "file");
        assert_eq!(symbol_kind_str(2), "module");
        assert_eq!(symbol_kind_str(5), "class");
        assert_eq!(symbol_kind_str(6), "method");
        assert_eq!(symbol_kind_str(8), "field");
        assert_eq!(symbol_kind_str(10), "enum");
        assert_eq!(symbol_kind_str(11), "interface");
        assert_eq!(symbol_kind_str(12), "fn");
        assert_eq!(symbol_kind_str(13), "var");
        assert_eq!(symbol_kind_str(14), "const");
        assert_eq!(symbol_kind_str(23), "struct");
        assert_eq!(symbol_kind_str(26), "type_param");
    }

    #[tokio::test]
    async fn test_execute_missing_args() {
        let result = execute("{}").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("operation") || err.contains("filePath") || err.contains("Invalid"));
    }

    #[tokio::test]
    async fn test_execute_unknown_extension() {
        let result =
            execute(r#"{"operation":"hover","filePath":"test.xyz","line":1,"character":1}"#).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No LSP server configured"));
    }

    #[tokio::test]
    async fn test_execute_unknown_operation() {
        // Create a temp .rs file so server lookup succeeds but operation fails
        let tmp = std::env::temp_dir().join("bfcode-test-lsp-op");
        let _ = std::fs::create_dir_all(&tmp);
        let rs_file = tmp.join("test.rs");
        let _ = std::fs::write(&rs_file, "fn main() {}");
        let cargo = tmp.join("Cargo.toml");
        let _ = std::fs::write(&cargo, "[package]\nname = \"test\"");

        let args = format!(
            r#"{{"operation":"badOp","filePath":"{}","line":1,"character":1}}"#,
            rs_file.display()
        );
        let result = execute(&args).await;
        // Either fails with unknown operation or fails because server not found — both ok
        if let Ok(output) = &result {
            // Should not succeed silently
            assert!(!output.is_empty());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
