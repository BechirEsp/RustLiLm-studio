# RustLiLm Studio

A native desktop image and video generation app written in **Rust + GPUI**, using your **LiteLLM gateway**.

## Getting started

1. Enter your LiteLLM gateway URL and API key (or set `LITELLM_API_KEY`) and connect.
2. Choose an output folder and optional project/date grouping.
3. Create or select a project in the **left sidebar**. The sidebar shows the project count; expand a project to see its submitted prompt history.
4. Write your prompt in the **bottom composer**. On its right, choose Image or Video, model, size, optional video duration, and Generate.
5. Click an image to open the **in-app viewer**. Copy it from the card or viewer; an **Image copied** toast appears at the top right. Close the viewer with its Close button or Escape.
6. Open **Settings** from the sidebar whenever you need it. **Save settings** applies changes. **Close**, **Cancel**, or **Escape** returns to the studio and discards unsaved edits, including an in-flight connection check.

API keys stay in memory and are never written to settings or history. Settings remember your gateway URL and output preferences. Use HTTPS for a remote gateway.

## Projects and prompt history

Projects are created and selected entirely in the left pane. Each accordion lists the prompts submitted in that project, with their time and result status. Select a prompt to restore its text, output type, model, size, and duration and view its generated outputs. Editing and generating again creates a new history entry. Failed requests remain in history too. Unsubmitted drafts are retained in memory when switching between projects.

The right pane contains the selected project's gallery and the bottom composer. Images open inside the app; videos open in your operating system's default player. Hover actions still provide **Copy image** and **Show in folder**.

## Output and metadata folders

Generated media follows the optional layout `chosen-folder/project/YYYY-MM-DD/time-uuid.png`. Project and date folders are independently optional. **Project folders contain media only.** All app JSON data is stored separately under the output folder:

| Path | Contents |
| --- | --- |
| `_studio/gallery.json` | Local output index |
| `_studio/projects.json` | Projects, prompt history, and generation settings |
| `_studio/assets/<id>.json` | Metadata for each generated image/video |
| `_studio/jobs/<id>.json` | Video jobs available for resuming |

On first use after upgrading from v0.1, the app migrates its old gallery index, recognized JSON sidecars, and pending-video checkpoints into `_studio`, while keeping the actual images/videos in place. Unrelated JSON files are left alone. Projects and prompt history are reconstructed from the old gallery. Historical sizes/durations unavailable in v0.1 metadata use the original defaults when restoring a legacy prompt.

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

If a video times out or the app closes, use **Resume saved video** and select its checkpoint from `_studio/jobs`. Resuming polls the existing job without submitting it again. **Stop waiting** stops local polling and does not cancel the remote job. A stopped or timed-out image request may also still complete remotely; check your gateway before retrying.

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
- `src/storage.rs`: settings, safe output paths, centralized metadata, and legacy migration.
- `src/projects.rs`: persistent projects and prompt history.
- `tests/gateway.rs`: local HTTP integration tests; no paid provider requests.
- `tests/projects.rs`: project/history persistence, output association, and migration regression tests.

## Current scope

Text-to-image and text-to-video generation. Image editing, input reference images, in-app video playback, provider-specific advanced parameters, and operating system credential vault storage are not yet included. The API key can be provided on every launch or through the environment.

MIT licensed. GPUI and GPUI Component retain their upstream Apache-2.0 licenses.
