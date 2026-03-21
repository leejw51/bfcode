//! Playwright-based browser integration tests for bfcode.
//!
//! These tests use playwright-rs to validate browser automation features.
//! They require the Playwright driver to be installed.
//!
//! Setup:
//!   1. The playwright crate bundles/unpacks the driver on first run.
//!   2. Install browsers: npx playwright install chromium
//!      (or the driver will use its bundled browsers)
//!
//! Run these tests explicitly:
//!   cargo test --test playwright_browser -- --ignored
//!
//! All tests are #[ignore] by default so `cargo test` / `make test` skip them.

use playwright::api::Viewport;
use playwright::Playwright;

/// Create a simple HTML test file and return its file:// URL
fn create_test_html(name: &str, html: &str) -> String {
    let dir = std::env::temp_dir().join("bfcode_playwright_tests");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{name}.html"));
    std::fs::write(&file, html).unwrap();
    format!("file://{}", file.display())
}

/// Clean up test HTML files
fn cleanup_test_files() {
    let dir = std::env::temp_dir().join("bfcode_playwright_tests");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Helper: initialize playwright and prepare driver
async fn setup() -> (Playwright, playwright::api::Browser) {
    let pw = Playwright::initialize().await.unwrap();
    pw.prepare().unwrap();
    let browser = pw
        .chromium()
        .launcher()
        .headless(true)
        .launch()
        .await
        .unwrap();
    (pw, browser)
}

// ============================================================
// Basic Browser Lifecycle Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_playwright_initialize() {
    let pw = Playwright::initialize().await.unwrap();
    pw.prepare().unwrap();
    // Verify we can access browser types
    let _chromium = pw.chromium();
    let _firefox = pw.firefox();
    let _webkit = pw.webkit();
}

#[tokio::test]
#[ignore]
async fn test_launch_and_close_browser() {
    let (_pw, browser) = setup().await;
    assert!(browser.exists());
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_create_context_and_page() {
    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    let url = page.url().unwrap();
    assert!(
        url == "about:blank" || url.is_empty(),
        "Expected about:blank, got: {url}"
    );

    browser.close().await.unwrap();
}

// ============================================================
// Navigation Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_navigate_to_page() {
    let url = create_test_html(
        "navigate",
        r#"<html><head><title>Test Page</title></head><body><h1>Hello</h1></body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let title: String = page.eval("() => document.title").await.unwrap();
    assert_eq!(title, "Test Page");

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_navigate_and_get_content() {
    let url = create_test_html(
        "content",
        r#"<html><body><div id="main">Hello World</div></body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let text: String = page
        .eval("() => document.getElementById('main').textContent")
        .await
        .unwrap();
    assert_eq!(text, "Hello World");

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_navigate_back_forward() {
    let url1 = create_test_html(
        "page1",
        "<html><head><title>Page 1</title></head><body></body></html>",
    );
    let url2 = create_test_html(
        "page2",
        "<html><head><title>Page 2</title></head><body></body></html>",
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url1).goto().await.unwrap();
    page.goto_builder(&url2).goto().await.unwrap();

    let title: String = page.eval("() => document.title").await.unwrap();
    assert_eq!(title, "Page 2");

    page.go_back_builder().go_back().await.unwrap();
    let title: String = page.eval("() => document.title").await.unwrap();
    assert_eq!(title, "Page 1");

    page.go_forward_builder().go_forward().await.unwrap();
    let title: String = page.eval("() => document.title").await.unwrap();
    assert_eq!(title, "Page 2");

    browser.close().await.unwrap();
}

// ============================================================
// JavaScript Evaluation Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_eval_simple_expression() {
    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    let result: i32 = page.eval("() => 2 + 3").await.unwrap();
    assert_eq!(result, 5);

    let result: String = page.eval("() => 'hello' + ' ' + 'world'").await.unwrap();
    assert_eq!(result, "hello world");

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_eval_dom_manipulation() {
    let url = create_test_html(
        "eval_dom",
        r#"<html><body><div id="target">original</div></body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    page.eval::<()>(
        "() => { document.getElementById('target').textContent = 'modified'; }",
    )
    .await
    .unwrap();

    let text: String = page
        .eval("() => document.getElementById('target').textContent")
        .await
        .unwrap();
    assert_eq!(text, "modified");

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_eval_returns_json() {
    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    let result: serde_json::Value = page
        .eval("() => ({ name: 'test', count: 42, items: [1, 2, 3] })")
        .await
        .unwrap();

    assert_eq!(result["name"], "test");
    assert_eq!(result["count"], 42);
    assert_eq!(result["items"].as_array().unwrap().len(), 3);

    browser.close().await.unwrap();
}

// ============================================================
// Click & Interaction Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_click_element() {
    let url = create_test_html(
        "click",
        r#"<html><body>
        <button id="btn" onclick="document.getElementById('result').textContent='clicked'">Click me</button>
        <div id="result">not clicked</div>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();
    page.click_builder("#btn").click().await.unwrap();

    let result: String = page
        .eval("() => document.getElementById('result').textContent")
        .await
        .unwrap();
    assert_eq!(result, "clicked");

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_click_link_navigation() {
    let target_url = create_test_html(
        "link_target",
        "<html><head><title>Target</title></head><body>Landed</body></html>",
    );

    let source_url = create_test_html(
        "link_source",
        &format!(
            r#"<html><body><a id="link" href="{target_url}">Go to target</a></body></html>"#
        ),
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&source_url).goto().await.unwrap();
    page.click_builder("#link").click().await.unwrap();

    // Wait for navigation
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let title: String = page.eval("() => document.title").await.unwrap();
    assert_eq!(title, "Target");

    browser.close().await.unwrap();
}

// ============================================================
// Text Input Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_type_into_input() {
    let url = create_test_html(
        "input",
        r#"<html><body>
        <input id="name" type="text" />
        <textarea id="desc"></textarea>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    // Type into input (note: typo in crate API — type_builer not type_builder)
    page.click_builder("#name").click().await.unwrap();
    page.type_builer("#name", "Hello World")
        .r#type()
        .await
        .unwrap();

    let value: String = page
        .eval("() => document.getElementById('name').value")
        .await
        .unwrap();
    assert_eq!(value, "Hello World");

    // Type into textarea
    page.click_builder("#desc").click().await.unwrap();
    page.type_builer("#desc", "Line 1\nLine 2")
        .r#type()
        .await
        .unwrap();

    let value: String = page
        .eval("() => document.getElementById('desc').value")
        .await
        .unwrap();
    assert!(value.contains("Line 1"));
    assert!(value.contains("Line 2"));

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_fill_input() {
    let url = create_test_html(
        "fill",
        r#"<html><body><input id="field" type="text" value="old" /></body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    // Fill replaces existing value
    page.fill_builder("#field", "new value")
        .fill()
        .await
        .unwrap();

    let value: String = page
        .eval("() => document.getElementById('field').value")
        .await
        .unwrap();
    assert_eq!(value, "new value");

    browser.close().await.unwrap();
}

// ============================================================
// Screenshot Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_screenshot_page() {
    let url = create_test_html(
        "screenshot",
        r#"<html><body style="background:red;width:100px;height:100px"><h1>Screenshot Test</h1></body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let screenshot_path = std::env::temp_dir().join("bfcode_pw_screenshot.png");
    let screenshot_bytes = page.screenshot_builder().screenshot().await.unwrap();

    assert!(!screenshot_bytes.is_empty(), "Screenshot should not be empty");
    std::fs::write(&screenshot_path, &screenshot_bytes).unwrap();
    assert!(screenshot_path.exists());

    // PNG magic bytes check
    assert_eq!(&screenshot_bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);

    let _ = std::fs::remove_file(&screenshot_path);
    browser.close().await.unwrap();
}

// ============================================================
// Selector / Query Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_query_selector() {
    let url = create_test_html(
        "selector",
        r#"<html><body>
        <ul>
            <li class="item">Item 1</li>
            <li class="item">Item 2</li>
            <li class="item">Item 3</li>
        </ul>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let count: i32 = page
        .eval("() => document.querySelectorAll('.item').length")
        .await
        .unwrap();
    assert_eq!(count, 3);

    let text: String = page
        .eval("() => document.querySelector('.item').textContent")
        .await
        .unwrap();
    assert_eq!(text, "Item 1");

    browser.close().await.unwrap();
}

// ============================================================
// Form Interaction Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_form_submission() {
    let url = create_test_html(
        "form",
        r#"<html><body>
        <form id="myform" onsubmit="event.preventDefault(); document.getElementById('status').textContent='submitted: ' + document.getElementById('email').value">
            <input id="email" type="text" name="email" />
            <button id="submit" type="submit">Submit</button>
        </form>
        <div id="status">waiting</div>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    page.fill_builder("#email", "test@example.com")
        .fill()
        .await
        .unwrap();
    page.click_builder("#submit").click().await.unwrap();

    let status: String = page
        .eval("() => document.getElementById('status').textContent")
        .await
        .unwrap();
    assert_eq!(status, "submitted: test@example.com");

    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_checkbox_interaction() {
    let url = create_test_html(
        "checkbox",
        r#"<html><body>
        <input id="agree" type="checkbox" />
        <label for="agree">I agree</label>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let checked: bool = page
        .eval("() => document.getElementById('agree').checked")
        .await
        .unwrap();
    assert!(!checked);

    page.check_builder("#agree").check().await.unwrap();

    let checked: bool = page
        .eval("() => document.getElementById('agree').checked")
        .await
        .unwrap();
    assert!(checked);

    browser.close().await.unwrap();
}

// ============================================================
// Multiple Pages / Tabs Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_multiple_pages() {
    let url1 = create_test_html(
        "multi1",
        "<html><head><title>Tab 1</title></head><body></body></html>",
    );
    let url2 = create_test_html(
        "multi2",
        "<html><head><title>Tab 2</title></head><body></body></html>",
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();

    let page1 = context.new_page().await.unwrap();
    let page2 = context.new_page().await.unwrap();

    page1.goto_builder(&url1).goto().await.unwrap();
    page2.goto_builder(&url2).goto().await.unwrap();

    let title1: String = page1.eval("() => document.title").await.unwrap();
    let title2: String = page2.eval("() => document.title").await.unwrap();

    assert_eq!(title1, "Tab 1");
    assert_eq!(title2, "Tab 2");

    browser.close().await.unwrap();
}

// ============================================================
// Viewport / Emulation Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_custom_viewport() {
    let (_pw, browser) = setup().await;

    let context = browser
        .context_builder()
        .viewport(Some(Viewport {
            width: 800,
            height: 600,
        }))
        .build()
        .await
        .unwrap();

    let page = context.new_page().await.unwrap();

    let width: i32 = page.eval("() => window.innerWidth").await.unwrap();
    let height: i32 = page.eval("() => window.innerHeight").await.unwrap();

    assert_eq!(width, 800);
    assert_eq!(height, 600);

    browser.close().await.unwrap();
}

// ============================================================
// Wait / Timing Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_wait_for_delayed_element() {
    let url = create_test_html(
        "wait",
        r#"<html><body>
        <div id="container"></div>
        <script>
            setTimeout(function() {
                var el = document.createElement('div');
                el.id = 'delayed';
                el.textContent = 'appeared';
                document.getElementById('container').appendChild(el);
            }, 200);
        </script>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    // Wait for the delayed element
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let text: String = page
        .eval("() => document.getElementById('delayed') ? document.getElementById('delayed').textContent : 'not found'")
        .await
        .unwrap();
    assert_eq!(text, "appeared");

    browser.close().await.unwrap();
}

// ============================================================
// Error Handling Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_click_nonexistent_element_error() {
    let url = create_test_html("empty", "<html><body></body></html>");

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let result = page
        .click_builder("#nonexistent")
        .timeout(1000.0)
        .click()
        .await;
    assert!(result.is_err(), "Expected error clicking nonexistent element");

    browser.close().await.unwrap();
}

// ============================================================
// Page Title Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_page_title() {
    let url = create_test_html(
        "title",
        "<html><head><title>My Test Title</title></head><body></body></html>",
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let title = page.title().await.unwrap();
    assert_eq!(title, "My Test Title");

    browser.close().await.unwrap();
}

// ============================================================
// Content / InnerHTML Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_inner_html_and_text() {
    let url = create_test_html(
        "innerhtml",
        r#"<html><body>
        <div id="container"><span class="bold">Hello</span> <em>World</em></div>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let html: String = page
        .eval("() => document.getElementById('container').innerHTML")
        .await
        .unwrap();
    assert!(html.contains("<span"));
    assert!(html.contains("Hello"));

    let text: String = page
        .eval("() => document.getElementById('container').innerText")
        .await
        .unwrap();
    assert!(text.contains("Hello"));
    assert!(text.contains("World"));

    browser.close().await.unwrap();
}

// ============================================================
// Reload Test
// ============================================================

#[tokio::test]
#[ignore]
async fn test_page_reload() {
    let url = create_test_html(
        "reload",
        r#"<html><body><div id="counter">0</div>
        <script>
            var c = parseInt(document.getElementById('counter').textContent) + 1;
            document.getElementById('counter').textContent = c;
        </script>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    page.goto_builder(&url).goto().await.unwrap();

    let val: String = page
        .eval("() => document.getElementById('counter').textContent")
        .await
        .unwrap();
    assert_eq!(val, "1");

    page.reload_builder().reload().await.unwrap();

    let val: String = page
        .eval("() => document.getElementById('counter').textContent")
        .await
        .unwrap();
    assert_eq!(val, "1"); // fresh page load, counter resets and increments to 1

    browser.close().await.unwrap();
}

// ============================================================
// Integration: Simulate bfcode browser tool flow
// ============================================================

#[tokio::test]
#[ignore]
async fn test_simulate_bfcode_browser_workflow() {
    // Simulate the workflow: navigate -> extract text -> click -> type -> screenshot
    let url = create_test_html(
        "workflow",
        r#"<html><head><title>bfcode Test App</title></head><body>
        <h1 id="heading">Welcome</h1>
        <input id="search" type="text" placeholder="Search..." />
        <button id="go" onclick="document.getElementById('heading').textContent='Searched: ' + document.getElementById('search').value">Go</button>
        </body></html>"#,
    );

    let (_pw, browser) = setup().await;
    let context = browser.context_builder().build().await.unwrap();
    let page = context.new_page().await.unwrap();

    // Step 1: Navigate
    page.goto_builder(&url).goto().await.unwrap();
    let title = page.title().await.unwrap();
    assert_eq!(title, "bfcode Test App");

    // Step 2: Extract content
    let heading: String = page
        .eval("() => document.getElementById('heading').textContent")
        .await
        .unwrap();
    assert_eq!(heading, "Welcome");

    // Step 3: Type into input
    page.fill_builder("#search", "rust playwright")
        .fill()
        .await
        .unwrap();

    // Step 4: Click button
    page.click_builder("#go").click().await.unwrap();

    // Step 5: Verify result
    let heading: String = page
        .eval("() => document.getElementById('heading').textContent")
        .await
        .unwrap();
    assert_eq!(heading, "Searched: rust playwright");

    // Step 6: Take screenshot
    let bytes = page.screenshot_builder().screenshot().await.unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..4], &[0x89, 0x50, 0x4E, 0x47]); // PNG header

    browser.close().await.unwrap();
    cleanup_test_files();
}
