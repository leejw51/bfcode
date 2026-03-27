"""Grok Image Generator - generates images using xAI's Grok Imagine API."""

import os
import sys
import webbrowser
import httpx
import shutil
from pathlib import Path

API_KEY = os.environ.get("GROK_API_KEY")
if not API_KEY:
    print("Error: GROK_API_KEY environment variable not set")
    sys.exit(1)

HEADERS = {"Authorization": f"Bearer {API_KEY}", "Content-Type": "application/json"}
IMAGE_URL = "https://api.x.ai/v1/images/generations"
MODEL = "grok-imagine-image"

# Exchange rates (approximate)
USD_TO_KRW = 1450
USD_TO_HKD = 7.82

# Output directory for saved images
OUTPUT_DIR = Path("generated_images")


def get_rate_limits(response_headers):
    """Extract rate limit info from response headers."""
    info = {}
    for key, val in response_headers.items():
        if "ratelimit" in key.lower() or "credit" in key.lower():
            info[key] = val
    return info


def generate_image(prompt):
    """Generate an image and return (data, headers)."""
    resp = httpx.post(
        IMAGE_URL,
        headers=HEADERS,
        json={
            "model": MODEL,
            "prompt": prompt,
            "n": 1,
            "response_format": "url",
        },
        timeout=120,
    )
    resp.raise_for_status()
    return resp.json(), dict(resp.headers)


def download_image(url, filename):
    """Download image from URL and save locally."""
    resp = httpx.get(url, timeout=60, follow_redirects=True)
    resp.raise_for_status()
    OUTPUT_DIR.mkdir(exist_ok=True)
    filepath = OUTPUT_DIR / filename
    filepath.write_bytes(resp.content)
    return filepath


def print_cost(usd, count):
    """Print cost in USD, KRW, and HKD."""
    total_usd = usd * count
    total_krw = total_usd * USD_TO_KRW
    total_hkd = total_usd * USD_TO_HKD

    print(f"\n--- Pricing (per xAI docs) ---")
    print(f"  Model: {MODEL}")
    print(f"  Cost per image (1k): $0.01 / {0.01 * USD_TO_KRW:.0f} KRW / HK${0.01 * USD_TO_HKD:.2f}")
    print(f"  Cost per image (2k): $0.04 / {0.04 * USD_TO_KRW:.0f} KRW / HK${0.04 * USD_TO_HKD:.2f}")
    print(f"  Images generated:    {count}")
    print(f"  Estimated total:     ${total_usd:.2f} / {total_krw:.0f} KRW / HK${total_hkd:.2f}")


def main():
    print("=" * 50)
    print(f"  Grok Image Generator  (model: {MODEL})")
    print("=" * 50)

    # Read prompt from user
    prompt = input("\nEnter image prompt: ").strip()
    if not prompt:
        print("No prompt provided. Exiting.")
        return

    # Generate image
    print(f"\nGenerating image...")
    try:
        result, headers = generate_image(prompt)
    except httpx.HTTPStatusError as e:
        print(f"Error: {e.response.status_code} - {e.response.text}")
        return

    # Show results
    images = result.get("data", [])
    print(f"\nDone! Generated {len(images)} image(s).\n")

    for i, img in enumerate(images):
        url = img.get("url", "")
        revised = img.get("revised_prompt", "")

        # Download and save locally
        if url:
            # Extract extension from URL
            ext = url.rsplit(".", 1)[-1].split("?")[0] if "." in url else "jpeg"
            filename = f"image_{i + 1}.{ext}"
            filepath = download_image(url, filename)
            abs_path = filepath.resolve()

            print(f"  Image {i + 1}:")
            print(f"    Temp URL:  {url}")
            print(f"    Saved to:  {abs_path}")
            if revised:
                print(f"    Revised prompt: {revised}")
            webbrowser.open(url)

    # Show rate limit info
    limits = get_rate_limits(headers)
    if limits:
        print(f"\n--- Rate Limits ---")
        for k, v in limits.items():
            label = k.replace("x-ratelimit-", "").replace("-", " ").title()
            print(f"  {label}: {v}")

    # Show usage from response body if present
    usage = result.get("usage")
    if usage:
        print(f"\n--- Usage ---")
        for k, v in usage.items():
            print(f"  {k}: {v}")

    # Show cost in USD, KRW, HKD
    print_cost(0.01, len(images))
    print()


if __name__ == "__main__":
    main()
