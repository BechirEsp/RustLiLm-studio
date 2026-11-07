# RustLiLm Studio

A native desktop image and video generation app written in **Rust + GPUI**, using your **LiteLLM gateway**.

## Getting started

1. Open the app and enter your gateway URL, with or without `/v1` (for example `http://localhost:4000`). Enter an API key if your gateway requires one, or set `LITELLM_API_KEY` before launching.
2. Connect to discover available models. Choose a model and an output directory.
3. Enable **Create a folder for each project** and/or **Group creations by date** if desired.
4. Open the studio, choose Images or Videos, select a model and size, and write your prompt.
5. Hover over an image for **Copy image** and **Show in folder**. Videos offer **Play** and **Show in folder**.

API keys remain in memory and are never written to settings, gallery metadata, or the repository. Settings remember your gateway URL and output preferences. Use HTTPS for a remote gateway.

## Features

- `/v1/models` discovery with optional `/model/info` capability metadata. Gateways that restrict `/model/info` still work. Aliases without capability metadata are available in both image and video mode; choose the appropriate alias yourself.
- Switch models for each generation. Image size presets and video duration/size controls.
- Images from either `b64_json` or downloadable URLs; PNG, JPEG, WebP, and GIF outputs retain their original format.
- Asynchronous video submission, progress polling, and saved job recovery. **Stop waiting** stops local polling; it does not cancel the remote job. **Resume video** opens the saved `pending-video.json` file and polls the same job without submitting again.
- Persistent local gallery, unique filenames, and a JSON sidecar with model, prompt, project, and timestamp for every output.
- Output layout: `chosen-folder/project/YYYY-MM-DD/time-uuid.png` (project/date parts are independently optional).
- Native folder picker and operating system clipboard/file manager integration.

## LiteLLM requirements

The gateway must expose the endpoints used by the selected models:

| Operation | Endpoint |
| --- | --- |
| Model discovery | `GET /v1/models` |
| Optional capability enrichment | `GET /model/info` |
| Image generation | `POST /v1/images/generations` (JSON) |
| Video generation | `POST /v1/videos` (multipart form) |
| Video status | `GET /v1/videos/{id}` |
| Video download | `GET /v1/videos/{id}/content` |

Model aliases, supported sizes, durations, and billing are controlled by your gateway/provider. Configure actual image/video generation deployments in LiteLLM; chat-only deployments cannot generate media through these endpoints. The UI exposes common size/duration presets, which may not be supported by every model. Video outputs must be MP4. Videos open in your default player rather than playing inside the gallery.

If a video times out or the app closes, find `pending-video.json` in the project's output folder and use **Resume video** with the same gateway. A pending job blocks another video submission into that project/date folder to avoid accidental double billing. For a permanently failed job, remove its JSON checkpoint manually before retrying. A stopped or timed-out image request may also still complete remotely; check your gateway before retrying.

## Build from source

Install stable Rust from [rustup.rs](https://rustup.rs/).

### Windows

Install Visual Studio Build Tools with **Desktop development with C++**, the Windows SDK, and CMake. Then:

```powershell
cargo run --locked --release
```

### Linux (Ubuntu 24.04 / Debian)

```sh
sudo apt install build-essential clang cmake pkg-config libclang-dev \
  libasound2-dev libfontconfig1-dev libfreetype6-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libwayland-dev libxcb1-dev libvulkan-dev libssl-dev
cargo run --locked --release
```

GPUI requires a desktop session and working GPU/Vulkan driver. Linux folder dialogs use the desktop portal; install your desktop's `xdg-desktop-portal` backend if the picker is unavailable.

### Tests and packaging

```sh
cargo test --locked --no-default-features  # Core + mock gateway tests, no display needed
cargo test --locked --all-targets         # Also compiles the native interface
cargo fmt --all -- --check
cargo clippy --locked --no-default-features --all-targets -- -D warnings
cargo build --locked --release
cargo install cargo-deb --locked
cargo deb --no-build
```

The GitHub workflow runs tests, builds natively on Windows and Ubuntu, packages `.exe` and `.deb` files, and publishes a versioned GitHub release only after both builds pass. Pushes to `main` publish a release when `Cargo.toml` contains a version that has not been released yet. Pull requests only test/build. A `v*` tag or manual workflow dispatch can also run the pipeline. The build produces unsigned binaries and SHA-256 checksums.

## Project structure

- `src/main.rs`: GPUI interface and background task coordination.
- `src/gateway.rs`: HTTP client, model discovery, image/video APIs, job checkpoints.
- `src/storage.rs`: settings, safe output paths, metadata, local gallery.
- `tests/gateway.rs`: local HTTP integration tests; no paid provider requests.

## Current scope

Text-to-image and text-to-video generation. Image editing, input reference images, in-app video playback, provider-specific advanced parameters, and operating system credential vault storage are not yet included. The API key can be provided on every launch or through the environment.

MIT licensed. GPUI and GPUI Component retain their upstream Apache-2.0 licenses.
