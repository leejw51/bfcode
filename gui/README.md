# The Never Ending Coding - GUI Client

A Love2D GUI client for the BFCode gateway server. Features a login screen and chat interface with animated sprite characters.

## Prerequisites

- [Love2D](https://love2d.org/) (v11.4+)
- A running BFCode gateway server (default: `http://127.0.0.1:8642`)

### Install Love2D

**macOS:**
```bash
brew install love
```

**Ubuntu/Debian:**
```bash
sudo apt install love
```

**Windows:**

Download the installer from https://love2d.org/

## Setup

1. Copy the example environment file and add your API key:
   ```bash
   cp .env.example .env
   ```

2. Edit `.env` and set your `GROK_API_KEY` (only needed for image generation).

## Run

```bash
love .
```

## Image Generation (Optional)

The `generate_image.py` script uses the Grok API to generate images. Requires the `GROK_API_KEY` environment variable to be set.

```bash
pip install requests pillow
python generate_image.py
```

## Sprite Processing (Optional)

To regenerate sprite sheets from individual walk frames:

```bash
pip install pillow
python process_sprites.py
```
