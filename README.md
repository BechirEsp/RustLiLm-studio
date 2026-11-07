# RustLiLm Studio

A native desktop image and video generation app written in **Rust + GPUI**, using **LiteLLM** or a compatible **Ollama** server.

## Getting started

1. Choose LiteLLM or Ollama, enter the server URL and API key if required, and connect. Local Ollama normally uses `http://localhost:11434` without a key.
2. Choose an output folder and optional project/date grouping.
3. Create or select a project in the **left sidebar**. The sidebar shows the project count; expand a project to see its submitted prompt history.
4. Write your prompt in the **bottom composer**. On its right, choose Image or Video, model, size, optional video duration, and Generate.
5. Click an image to open the **in-app viewer**. Copy it from the card or viewer; an **Image copied** toast appears at the top right. Close the viewer with its Close button or Escape.
6. Open **Settings** from the sidebar whenever you need it. **Save settings** applies changes. **Close**, **Cancel**, or **Escape** returns to the studio and discards unsaved edits, including an in-flight connection check.

**Remember API key** is enabled by default. On a successful connection the app saves the key to the operating system credential store (Windows Credential Manager, macOS Keychain, or a Linux Secret Service such as GNOME Keyring/KWallet). On later launches it restores the key into the masked field. Keys are scoped to the provider and server URL and are **not stored in SQLite or prompt history**. Uncheck Remember to remove the saved credential after applying the connection. If the credential store is unavailable/locked, the app reports that saving failed and keeps the current connection usable; unlock the store or use `LITELLM_API_KEY`/`OLLAMA_API_KEY`.

Connection settings and output preferences are saved in SQLite in the app's configuration directory. Users upgrading from a version that never stored credentials must enter their key once, then connect with Remember enabled. Use HTTPS for remote servers.

## Edit an image with a prompt

Use **Edit** on a gallery image or **Edit image** in its viewer. To edit an external file, use **Upload image…** in the composer. A reference thumbnail appears beside a Remove button. Describe the changes, choose an image-edit-capable LiteLLM model, and click **Edit image**.

The source is copied into `_studio/inputs` and normalized to PNG. Originals are never overwritten. Edited outputs are new files, and history remembers the reference image and generation settings. **New prompt** or Remove returns to text-to-image generation. PNG, JPEG, WebP and GIF uploads are accepted (GIF editing uses the decoded first frame). Input files are limited to 100 MiB, with a 512 MiB decoder allocation limit.

## Ollama integration and model catalog

Choose Ollama on the connection screen or in Settings. The selector combines installed image-generation models from `GET /api/tags` with the verified experimental models in `resources/ollama-models.json`. Installed models are marked **Installed**; missing catalog entries are marked **Download**. Selecting a missing model automatically calls `POST /api/pull` and shows streamed layer progress. Downloads happen on the connected Ollama server and can be several GB. Stop waiting unlocks the UI; selecting the model again checks/resumes through Ollama.

The bundled catalog currently includes `x/z-image-turbo:latest`, `x/flux2-klein:latest` and `x/flux2-klein:9b`, with links to their official model pages and licenses. Additional installed models advertising the `image` capability are discovered automatically. Vision/chat models are not presented as image generators.

**Current upstream limitation:** Ollama's [v0.32.6 release notes](https://github.com/ollama/ollama/releases/tag/v0.32.6) say experimental image generation was temporarily removed and identify v0.32.5 as the supported release. The [official image model pages](https://ollama.com/blog/image-generation) describe macOS support. The app supports the experimental `POST /api/generate` image response implemented in v0.32.5 and the official SDK, but a current upstream server may reject generation even after a successful download. The client can connect to a compatible remote server from Windows/Linux; this does not add image generation to an unsupported Ollama server.

There is no supported Ollama video-generation or image-edit API integrated here. Use LiteLLM for those operations. No documented exhaustive remote catalog API for all downloadable image/video generators was found; the selector therefore uses a source-linked verified catalog plus actual server capabilities instead of presenting incompatible Hugging Face/vision models as downloadable Ollama generators.

Sources: [Ollama image announcement](https://ollama.com/blog/image-generation), [Z-Image](https://ollama.com/x/z-image-turbo), [FLUX.2 Klein](https://ollama.com/x/flux2-klein), [Ollama API](https://docs.ollama.com/api), and the [official SDK's image-generation types](https://github.com/ollama/ollama-python/blob/main/ollama/_types.py).

## Projects and prompt history

Projects are created and selected entirely in the left pane. Each accordion lists the prompts submitted in that project, with their time and result status. Select a prompt to restore its text, output type, model, size, and duration and view its generated outputs. Editing and generating again creates a new history entry. Failed requests remain in history too. Unsubmitted drafts are retained in memory when switching between projects.

The right pane contains the selected project's gallery and the bottom composer. Images open inside the app; videos open in your operating system's default player. Hover actions still provide **Copy image** and **Show in folder**.

## SQLite storage and output folders

Generated media follows the optional layout `chosen-folder/project/YYYY-MM-DD/time-uuid.png`. Project and date folders are independently optional. **Project folders contain media only.** App state lives separately:

| Location | Contents |
| --- | --- |
| `_studio/studio.sqlite3` | Projects, prompt history, output metadata and pending video jobs |
| `_studio/inputs/<id>.png` | Preserved reference images used for editing |
| OS app configuration directory: `settings.sqlite3` | Connection endpoints, provider, output preferences and Remember setting |
| OS credential store | API keys, separate from the database |

SQLite uses WAL mode, transactions, indexed project/prompt/output tables, and idempotent upserts. Back up the `_studio` directory with the app closed, or use SQLite's backup mechanism; do not copy only the main database file while its WAL is active.

Existing v0.1/v0.2 JSON settings, gallery indexes, recognized sidecars, project/prompt history and video checkpoints are imported automatically. Recognized JSON files are removed only after their data is committed. Media files stay in place and unrelated JSON files are left alone. Legacy sizes/durations absent from old metadata use the original defaults when restoring an old prompt.

## LiteLLM requirements

The gateway must expose the endpoints used by the selected models:

| Operation | Endpoint |
| --- | --- |
| Model discovery | `GET /v1/models` |
| Optional capability enrichment | `GET /model/info` |
| Image generation | `POST /v1/images/generations` (JSON) |
| Prompt-based image editing | `POST /v1/images/edits` (multipart image + prompt) |
| Video generation | `POST /v1/videos` (multipart form) |
| Video status | `GET /v1/videos/{id}` |
| Video download | `GET /v1/videos/{id}/content` |

Model aliases, supported sizes, durations, and billing are controlled by your gateway/provider. Configure actual image/video generation deployments in LiteLLM; chat-only deployments cannot generate media through these endpoints. The UI exposes common size/duration presets, which may not be supported by every model. Video outputs must be MP4. Videos open in your default player rather than playing inside the gallery.

If a video times out or the app closes, use **Resume saved video** and choose a pending job from the in-app list (stored in SQLite). Connect to the LiteLLM gateway that originally created the job. Resuming polls the existing job without submitting it again. **Stop waiting** stops local polling and does not cancel the remote job. A stopped or timed-out image request may also still complete remotely; check your gateway before retrying.

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

GPUI requires a desktop session and working GPU/Vulkan driver. API-key persistence on Linux also requires an unlocked Secret Service implementation such as GNOME Keyring or KWallet on the session D-Bus. Linux folder dialogs use the desktop portal; install your desktop's `xdg-desktop-portal` backend if the picker is unavailable.

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
- `src/gateway.rs`: LiteLLM image generation/editing and video APIs.
- `src/ollama.rs`: native image API, capability discovery and streamed downloads.
- `src/database.rs`: SQLite schema and persistence.
- `src/credentials.rs`: OS credential-store integration.
- `src/storage.rs`: settings, safe output paths, centralized metadata, and legacy migration.
- `src/projects.rs`: persistent projects and prompt history.
- `tests/gateway.rs`: local HTTP integration tests; no paid provider requests.
- `tests/projects.rs`: project/history persistence and migration regression tests.
- `tests/database.rs`: SQLite, reference preservation, credential identities and a native Windows credential round trip.

## Current scope

Text-to-image, prompt-based image editing, and text-to-video via LiteLLM; experimental image generation on compatible Ollama servers. In-app video playback, editing masks, multiple input references and provider-specific advanced parameters are not yet included. Live generation still requires your own gateway/server; the HTTP flows are tested with local mock servers.

MIT licensed. GPUI and GPUI Component retain their upstream Apache-2.0 licenses.
