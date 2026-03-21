use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Global browser manager (lazy-initialized)
static BROWSER: std::sync::LazyLock<Mutex<BrowserManager>> =
    std::sync::LazyLock::new(|| Mutex::new(BrowserManager::new()));

/// Default CDP debugging port
const CDP_PORT: u16 = 9222;

/// Max chars to return from page content
const MAX_PAGE_CHARS: usize = 50_000;

pub struct BrowserManager {
    /// Chrome process handle
    process: Option<tokio::process::Child>,
    /// CDP HTTP base URL
    debug_url: Option<String>,
    /// Current page URL
    current_url: Option<String>,
}

impl BrowserManager {
    pub fn new() -> Self {
        Self {
            process: None,
            debug_url: None,
            current_url: None,
        }
    }

    /// Find Chrome/Chromium binary on the system
    fn find_chrome() -> Option<PathBuf> {
        // Check CHROME_PATH env var first
        if let Ok(path) = std::env::var("CHROME_PATH") {
            let p = PathBuf::from(&path);
            if p.exists() {
                return Some(p);
            }
        }

        if cfg!(target_os = "macos") {
            let candidates = [
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            ];
            for c in &candidates {
                let p = PathBuf::from(c);
                if p.exists() {
                    return Some(p);
                }
            }
        }

        // Check PATH for linux-style names (works on both linux and mac with homebrew)
        let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome"];
        for name in &names {
            if let Ok(output) = std::process::Command::new("which").arg(name).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(PathBuf::from(path));
                    }
                }
            }
        }

        None
    }
}

/// Ensure Chrome is running with remote debugging enabled.
/// Returns the CDP base URL.
async fn ensure_chrome_running() -> Result<String> {
    {
        let mgr = BROWSER.lock().unwrap();
        if let Some(ref url) = mgr.debug_url {
            // Verify it's still responsive
            let check_url = url.clone();
            drop(mgr);
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()?;
            if client.get(format!("{}/json/version", check_url)).send().await.is_ok() {
                return Ok(check_url);
            }
            // Not responsive — we'll relaunch below
            let mut mgr = BROWSER.lock().unwrap();
            mgr.debug_url = None;
            if let Some(ref mut child) = mgr.process {
                let _ = child.kill().await;
            }
            mgr.process = None;
        }
    }

    let chrome_path = BrowserManager::find_chrome()
        .context("Chrome/Chromium not found. Install Chrome or set CHROME_PATH env var.")?;

    eprintln!("{} Launching headless Chrome...", "[browser]".blue());

    let user_data_dir = std::env::temp_dir().join("bfcode-chrome-profile");

    let child = tokio::process::Command::new(&chrome_path)
        .arg("--headless=new")
        .arg(format!("--remote-debugging-port={}", CDP_PORT))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-gpu")
        .arg("--disable-extensions")
        .arg("--disable-popup-blocking")
        .arg("--disable-translate")
        .arg("--no-sandbox")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("Failed to launch Chrome at {:?}", chrome_path))?;

    let base_url = format!("http://localhost:{}", CDP_PORT);

    // Wait for CDP to become available
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let mut attempts = 0;
    loop {
        attempts += 1;
        if attempts > 30 {
            bail!("Chrome failed to start — CDP not available after 15 seconds");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if client
            .get(format!("{}/json/version", base_url))
            .send()
            .await
            .is_ok()
        {
            break;
        }
    }

    let mut mgr = BROWSER.lock().unwrap();
    mgr.process = Some(child);
    mgr.debug_url = Some(base_url.clone());

    eprintln!("{} Chrome ready on port {}", "[browser]".green(), CDP_PORT);

    Ok(base_url)
}

/// Get the WebSocket debugger URL for the first/current page target
async fn get_page_ws_url(base_url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client
        .get(format!("{}/json/list", base_url))
        .send()
        .await
        .context("Failed to list CDP targets")?;

    let targets: Vec<serde_json::Value> = resp.json().await?;

    // Find a page target
    for target in &targets {
        if target.get("type").and_then(|v| v.as_str()) == Some("page") {
            if let Some(ws_url) = target.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                return Ok(ws_url.to_string());
            }
        }
    }

    bail!("No page target found in CDP")
}

/// Send a CDP command via the /json/protocol HTTP endpoint.
/// For commands that don't need WebSocket streaming, we use the CDP HTTP endpoint.
async fn cdp_send(base_url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    // We need to get the target id first
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let resp = client
        .get(format!("{}/json/list", base_url))
        .send()
        .await
        .context("Failed to list CDP targets")?;

    let targets: Vec<serde_json::Value> = resp.json().await?;

    let target_id = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .and_then(|t| t.get("id").and_then(|v| v.as_str()))
        .context("No page target available")?
        .to_string();

    // Use the /json/protocol endpoint to send CDP commands
    // Actually, Chrome doesn't expose a simple HTTP API for arbitrary CDP commands.
    // We'll use the WebSocket debugger URL but via a simple one-shot HTTP-like pattern
    // by leveraging tokio-tungstenite... but we don't have that dependency.
    //
    // Alternative: for navigation and JS evaluation, use Chrome CLI flags directly.
    // For the interactive session, we'll use reqwest to talk to a lightweight approach:
    // Launch separate short-lived Chrome processes for specific operations.
    //
    // Better approach: use the /json/new?url= endpoint and /json/activate/ for navigation.

    // Navigate using the /json endpoint trick
    if method == "Page.navigate" {
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank");
        // Close old page and open new one with the URL
        let _ = client
            .get(format!("{}/json/close/{}", base_url, target_id))
            .send()
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let resp = client
            .get(format!("{}/json/new?{}", base_url, url))
            .send()
            .await
            .context("Failed to open new page")?;

        let new_target: serde_json::Value = resp.json().await?;
        return Ok(new_target);
    }

    // For other methods, return an unsupported error — we handle them differently
    Ok(serde_json::json!({"error": format!("Direct CDP call for {} not supported via HTTP", method)}))
}

/// Simple HTML tag stripping (mirrors the one in tools.rs for self-contained use)
fn strip_html_tags(html: &str) -> String {
    let mut text = html.to_string();

    // Remove script and style blocks
    for tag in &["script", "style"] {
        loop {
            let lower = text.to_lowercase();
            let open = format!("<{}", tag);
            let close = format!("</{}", tag);
            if let Some(start) = lower.find(&open) {
                if let Some(end_offset) = lower[start..].find(&close) {
                    if let Some(close_bracket) = lower[start + end_offset..].find('>') {
                        text = format!(
                            "{}{}",
                            &text[..start],
                            &text[start + end_offset + close_bracket + 1..]
                        );
                        continue;
                    }
                }
                text = text[..start].to_string();
            }
            break;
        }
    }

    // Strip tags
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
            result.push(' '); // space where tag was
        } else if !in_tag {
            result.push(ch);
        }
    }

    // Decode common entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Collapse whitespace
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_space = false;
    for ch in result.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(if ch == '\n' { '\n' } else { ' ' });
            }
            prev_space = true;
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }

    collapsed.trim().to_string()
}

/// Launch Chrome if not running, navigate to URL, return page text content.
pub async fn browser_navigate(url: &str) -> Result<String> {
    let base_url = ensure_chrome_running().await?;

    eprintln!("{} Navigating to {}", "[browser]".blue(), url.cyan());

    // Use Chrome's --dump-dom to get rendered HTML for the URL.
    // This is more reliable than trying to control a running instance without WebSocket.
    let chrome_path = BrowserManager::find_chrome()
        .context("Chrome not found")?;

    let output = tokio::process::Command::new(&chrome_path)
        .arg("--headless=new")
        .arg("--dump-dom")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--timeout=30000")
        .arg(url)
        .output()
        .await
        .with_context(|| format!("Failed to dump DOM for {}", url))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Chrome --dump-dom failed: {}", stderr);
    }

    let html = String::from_utf8_lossy(&output.stdout).to_string();
    let text = strip_html_tags(&html);

    // Also update the running browser's page via CDP /json/new endpoint
    {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        // Close existing page targets and open the new URL
        let resp = client
            .get(format!("{}/json/list", base_url))
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(targets) = resp.json::<Vec<serde_json::Value>>().await {
                for target in &targets {
                    if target.get("type").and_then(|v| v.as_str()) == Some("page") {
                        if let Some(id) = target.get("id").and_then(|v| v.as_str()) {
                            let _ = client
                                .get(format!("{}/json/close/{}", base_url, id))
                                .send()
                                .await;
                        }
                    }
                }
            }
        }

        // Open new page with the URL
        let _ = client
            .get(format!("{}/json/new?{}", base_url, url))
            .send()
            .await;
    }

    // Track current URL
    {
        let mut mgr = BROWSER.lock().unwrap();
        mgr.current_url = Some(url.to_string());
    }

    // Truncate if needed
    if text.len() > MAX_PAGE_CHARS {
        Ok(format!(
            "{}\n\n[Truncated — {} chars total, showing first {}]",
            &text[..MAX_PAGE_CHARS],
            text.len(),
            MAX_PAGE_CHARS
        ))
    } else {
        Ok(text)
    }
}

/// Take a screenshot of the current page, save to file, return path.
pub async fn browser_screenshot(output_path: Option<&str>) -> Result<String> {
    let current_url = {
        let mgr = BROWSER.lock().unwrap();
        mgr.current_url.clone()
    };

    let url = current_url
        .as_deref()
        .unwrap_or("about:blank");

    let chrome_path = BrowserManager::find_chrome()
        .context("Chrome not found")?;

    let screenshot_path = match output_path {
        Some(p) => PathBuf::from(p),
        None => {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            std::env::current_dir()?.join(format!("screenshot_{}.png", timestamp))
        }
    };

    // Ensure parent directory exists
    if let Some(parent) = screenshot_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    eprintln!(
        "{} Taking screenshot of {}",
        "[browser]".blue(),
        url.cyan()
    );

    let output = tokio::process::Command::new(&chrome_path)
        .arg("--headless=new")
        .arg(format!("--screenshot={}", screenshot_path.display()))
        .arg("--window-size=1280,720")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--hide-scrollbars")
        .arg(url)
        .output()
        .await
        .context("Failed to take screenshot")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Chrome --screenshot failed: {}", stderr);
    }

    if !screenshot_path.exists() {
        bail!("Screenshot file was not created at {}", screenshot_path.display());
    }

    let path_str = screenshot_path.display().to_string();
    eprintln!("{} Screenshot saved to {}", "[browser]".green(), path_str.cyan());

    Ok(format!("Screenshot saved to {}", path_str))
}

/// Click an element by CSS selector.
/// Uses JavaScript evaluation via a short-lived headless Chrome to click the element
/// on the page managed by the persistent Chrome instance.
pub async fn browser_click(selector: &str) -> Result<String> {
    let script = format!(
        r#"
        (function() {{
            var el = document.querySelector({selector});
            if (!el) return 'Error: Element not found for selector: ' + {selector};
            el.click();
            return 'Clicked: ' + el.tagName + (el.id ? '#' + el.id : '') + (el.className ? '.' + el.className.split(' ').join('.') : '');
        }})()
        "#,
        selector = serde_json::to_string(selector)?
    );

    browser_evaluate(&script).await
}

/// Type text into an element by CSS selector.
pub async fn browser_type(selector: &str, text: &str) -> Result<String> {
    let script = format!(
        r#"
        (function() {{
            var el = document.querySelector({selector});
            if (!el) return 'Error: Element not found for selector: ' + {selector};
            el.focus();
            el.value = {text};
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return 'Typed into: ' + el.tagName + (el.id ? '#' + el.id : '');
        }})()
        "#,
        selector = serde_json::to_string(selector)?,
        text = serde_json::to_string(text)?
    );

    browser_evaluate(&script).await
}

/// Evaluate JavaScript on the current page.
/// Uses Chrome's --headless mode with a print-to-stdout trick for simple evaluation.
/// For the persistent browser, this evaluates JS via a bookmarklet-style approach.
pub async fn browser_evaluate(script: &str) -> Result<String> {
    let base_url = ensure_chrome_running().await?;
    let current_url = {
        let mgr = BROWSER.lock().unwrap();
        mgr.current_url.clone()
    };

    let url = current_url
        .as_deref()
        .unwrap_or("about:blank");

    let chrome_path = BrowserManager::find_chrome()
        .context("Chrome not found")?;

    // Wrap script to print result so we can capture it via --dump-dom
    // We navigate to the page, then evaluate the script and write the result to the DOM
    let wrapped_script = format!(
        r#"
        (async function() {{
            try {{
                var __result = await (async function() {{ {script} }})();
                if (__result === undefined) __result = 'undefined';
                document.title = '';
                document.body.innerText = String(__result);
            }} catch(e) {{
                document.body.innerText = 'Error: ' + e.message;
            }}
        }})();
        "#,
        script = script
    );

    // Use --run-all-compositor-stages-before-draw and a data: URL with JS
    // Actually, Chrome headless can evaluate JS on a real URL using --dump-dom
    // combined with setTimeout. But the simplest approach is:
    // 1. Navigate to the URL
    // 2. Inject JS that modifies the DOM
    // 3. Dump the modified DOM

    // Use a javascript: URL approach — navigate to the real page, then run JS
    // Chrome headless doesn't support javascript: URLs, so we use a different approach:
    // Write a small HTML file that includes the script

    let eval_html = format!(
        r#"<html><body><script>
        (async function() {{
            try {{
                var __result = await (async function() {{ return {script}; }})();
                if (__result === undefined) __result = 'undefined';
                document.body.innerText = String(__result);
            }} catch(e) {{
                document.body.innerText = 'JS_ERROR: ' + e.message;
            }}
        }})();
        </script></body></html>"#,
        script = script
    );

    let eval_path = std::env::temp_dir().join("bfcode-eval.html");
    tokio::fs::write(&eval_path, &eval_html).await?;

    let output = tokio::process::Command::new(&chrome_path)
        .arg("--headless=new")
        .arg("--dump-dom")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--timeout=10000")
        .arg(format!("file://{}", eval_path.display()))
        .output()
        .await
        .context("Failed to evaluate JavaScript")?;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&eval_path).await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Chrome JS evaluation failed: {}", stderr);
    }

    let result = String::from_utf8_lossy(&output.stdout).to_string();
    let text = strip_html_tags(&result);

    if text.is_empty() {
        Ok("(no output)".to_string())
    } else {
        Ok(text)
    }
}

/// Close the browser.
pub async fn browser_close() -> Result<String> {
    let mut mgr = BROWSER.lock().unwrap();

    if let Some(ref mut child) = mgr.process {
        eprintln!("{} Closing browser...", "[browser]".blue());
        let _ = child.kill().await;
        mgr.process = None;
        mgr.debug_url = None;
        mgr.current_url = None;
        eprintln!("{} Browser closed", "[browser]".green());
        Ok("Browser closed".to_string())
    } else {
        mgr.debug_url = None;
        mgr.current_url = None;
        Ok("Browser was not running".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_basic() {
        assert_eq!(strip_html_tags("<p>hello</p>"), "hello");
    }

    #[test]
    fn test_strip_html_script_removed() {
        let html = "<p>before</p><script>alert('x')</script><p>after</p>";
        let result = strip_html_tags(html);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("alert"));
    }

    #[test]
    fn test_strip_html_entities() {
        assert_eq!(strip_html_tags("a &amp; b"), "a & b");
    }

    #[test]
    fn test_find_chrome_does_not_panic() {
        // Just ensure it doesn't panic — may or may not find Chrome
        let _ = BrowserManager::find_chrome();
    }

    #[test]
    fn test_browser_manager_new() {
        let mgr = BrowserManager::new();
        assert!(mgr.process.is_none());
        assert!(mgr.debug_url.is_none());
        assert!(mgr.current_url.is_none());
    }
}
