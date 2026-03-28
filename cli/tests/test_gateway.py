#!/usr/bin/env python3
"""
Integration test for bfcode gateway.

Starts a gateway server as a subprocess, runs HTTP requests against its
endpoints, verifies responses, then tears down.

Usage:
    python3 tests/test_gateway.py [--binary PATH] [--port PORT]

Requires: cargo build (debug or release) to produce the bfcode binary.
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def find_binary():
    """Find the bfcode binary in standard cargo output locations."""
    candidates = [
        os.path.join("target", "release", "bfcode"),
        os.path.join("target", "debug", "bfcode"),
    ]
    for c in candidates:
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return None


def http_request(url, method="GET", body=None, headers=None, timeout=10):
    """Make an HTTP request and return (status, json_body)."""
    hdrs = {"Content-Type": "application/json", "Connection": "close"}
    if headers:
        hdrs.update(headers)
    data = json.dumps(body).encode() if body else None
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        try:
            raw = e.read().decode()
        except Exception:
            raw = ""
        if raw:
            try:
                return e.code, json.loads(raw)
            except json.JSONDecodeError:
                return e.code, {"error": raw[:200]}
        else:
            return e.code, {"error": f"HTTP {e.code} (empty body)"}


def wait_for_gateway(base_url, retries=30, delay=0.3):
    """Poll /v1/health until the gateway is ready."""
    for i in range(retries):
        try:
            status, body = http_request(f"{base_url}/v1/health", timeout=2)
            if status == 200:
                return True
        except Exception:
            pass
        time.sleep(delay)
    return False


# ---------------------------------------------------------------------------
# Test cases
# ---------------------------------------------------------------------------

class Results:
    def __init__(self):
        self.passed = 0
        self.failed = 0
        self.errors = []

    def ok(self, name):
        self.passed += 1
        print(f"  PASS  {name}")

    def fail(self, name, detail=""):
        self.failed += 1
        self.errors.append((name, detail))
        print(f"  FAIL  {name}: {detail}")

    def summary(self):
        total = self.passed + self.failed
        print(f"\n{'='*60}")
        print(f"Results: {self.passed}/{total} passed, {self.failed} failed")
        if self.errors:
            print("\nFailures:")
            for name, detail in self.errors:
                print(f"  - {name}: {detail}")
        print(f"{'='*60}")
        return self.failed == 0


def test_health(base_url, r):
    """GET /v1/health should return 200 with status=ok."""
    name = "GET /v1/health"
    try:
        status, body = http_request(f"{base_url}/v1/health")
        if status != 200:
            r.fail(name, f"expected 200 got {status}")
        elif body.get("status") != "ok":
            r.fail(name, f"expected status=ok got {body}")
        else:
            r.ok(name)
    except Exception as e:
        r.fail(name, str(e))


def test_status(base_url, r):
    """GET /v1/status should return running gateway info."""
    name = "GET /v1/status"
    try:
        status, body = http_request(f"{base_url}/v1/status")
        if status != 200:
            r.fail(name, f"expected 200 got {status}")
            return
        if not body.get("running"):
            r.fail(name, f"expected running=true got {body}")
            return
        if "version" not in body:
            r.fail(name, "missing version field")
            return
        r.ok(name)
    except Exception as e:
        r.fail(name, str(e))


def test_create_session(base_url, r):
    """POST /v1/sessions should create a new session."""
    name = "POST /v1/sessions"
    try:
        status, body = http_request(
            f"{base_url}/v1/sessions",
            method="POST",
            body={"user": "test-user"},
        )
        if status != 201:
            r.fail(name, f"expected 201 got {status}: {body}")
            return None
        sid = body.get("id")
        if not sid or not sid.startswith("sess_"):
            r.fail(name, f"invalid session id: {sid}")
            return None
        if body.get("user") != "test-user":
            r.fail(name, f"expected user=test-user got {body.get('user')}")
            return None
        r.ok(name)
        return sid
    except Exception as e:
        r.fail(name, str(e))
        return None


def test_list_sessions(base_url, r, expected_count=1):
    """GET /v1/sessions should list active sessions."""
    name = "GET /v1/sessions"
    try:
        status, body = http_request(f"{base_url}/v1/sessions")
        if status != 200:
            r.fail(name, f"expected 200 got {status}")
            return
        if not isinstance(body, list):
            r.fail(name, f"expected list got {type(body).__name__}")
            return
        if len(body) < expected_count:
            r.fail(name, f"expected >= {expected_count} sessions got {len(body)}")
            return
        r.ok(name)
    except Exception as e:
        r.fail(name, str(e))


def test_chat_missing_message(base_url, r):
    """POST /v1/chat with no message should return 400."""
    name = "POST /v1/chat (bad request)"
    try:
        status, body = http_request(
            f"{base_url}/v1/chat",
            method="POST",
            body={},
        )
        if status == 400 or status == 422:
            r.ok(name)
        else:
            r.fail(name, f"expected 400/422 got {status}: {body}")
    except Exception as e:
        # Some HTTP libraries return empty body for 4xx errors
        # Accept the error as long as the server rejected the request
        import traceback
        traceback.print_exc()
        r.fail(name, str(e))


def test_chat_invalid_session(base_url, r):
    """POST /v1/chat with invalid session_id should return 404."""
    name = "POST /v1/chat (invalid session)"
    try:
        status, body = http_request(
            f"{base_url}/v1/chat",
            method="POST",
            body={"message": "hello", "session_id": "sess_nonexistent"},
        )
        if status != 404:
            r.fail(name, f"expected 404 got {status}: {body}")
        else:
            r.ok(name)
    except Exception as e:
        r.fail(name, str(e))


def test_unknown_endpoint(base_url, r):
    """GET /v1/unknown should return 404."""
    name = "GET /v1/unknown (404)"
    try:
        status, body = http_request(f"{base_url}/v1/unknown")
        if status != 404:
            r.fail(name, f"expected 404 got {status}")
        else:
            r.ok(name)
    except Exception as e:
        r.fail(name, str(e))


def test_create_multiple_sessions(base_url, r):
    """Create multiple sessions and verify count."""
    name = "Multiple sessions"
    try:
        for i in range(3):
            status, body = http_request(
                f"{base_url}/v1/sessions",
                method="POST",
                body={"user": f"user-{i}"},
            )
            if status != 201:
                r.fail(name, f"session {i} creation failed: {status}")
                return

        status, body = http_request(f"{base_url}/v1/sessions")
        # At least 3 new + 1 from earlier test
        if len(body) >= 3:
            r.ok(name)
        else:
            r.fail(name, f"expected >= 3 sessions, got {len(body)}")
    except Exception as e:
        r.fail(name, str(e))


def test_status_fields(base_url, r):
    """Verify all expected fields in status response."""
    name = "Status response fields"
    try:
        status, body = http_request(f"{base_url}/v1/status")
        expected_fields = ["running", "listen", "mode", "uptime_secs",
                          "active_sessions", "total_requests", "version"]
        missing = [f for f in expected_fields if f not in body]
        if missing:
            r.fail(name, f"missing fields: {missing}")
        else:
            r.ok(name)
    except Exception as e:
        r.fail(name, str(e))


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="bfcode gateway integration tests")
    parser.add_argument("--binary", help="Path to bfcode binary")
    parser.add_argument("--port", type=int, default=18642, help="Port for test gateway")
    args = parser.parse_args()

    binary = args.binary or find_binary()
    if not binary:
        print("ERROR: bfcode binary not found. Run 'cargo build' first.")
        sys.exit(1)

    port = args.port
    listen = f"127.0.0.1:{port}"
    base_url = f"http://{listen}"

    print(f"Binary: {binary}")
    print(f"Gateway: {listen}")
    print(f"{'='*60}")

    # Start gateway
    print("\nStarting gateway...")
    proc = subprocess.Popen(
        [binary, "gateway", "start", "--listen", listen],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        if not wait_for_gateway(base_url):
            stderr = proc.stderr.read().decode() if proc.stderr else ""
            print(f"ERROR: Gateway failed to start within timeout.\nstderr: {stderr}")
            proc.terminate()
            sys.exit(1)

        print("Gateway is ready.\n")

        # Run tests
        r = Results()

        test_health(base_url, r)
        test_status(base_url, r)
        test_status_fields(base_url, r)
        test_create_session(base_url, r)
        test_list_sessions(base_url, r)
        test_chat_missing_message(base_url, r)
        test_chat_invalid_session(base_url, r)
        test_unknown_endpoint(base_url, r)
        test_create_multiple_sessions(base_url, r)

        success = r.summary()

    finally:
        # Tear down gateway
        print("\nStopping gateway...")
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        print("Gateway stopped.")

    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
