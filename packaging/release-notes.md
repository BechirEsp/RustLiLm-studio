RustLiLm Studio v0.3.0 adds image editing, Ollama integration, SQLite and remembered API keys.

- Edit a gallery image or upload a reference image, describe changes, and generate an edited copy through LiteLLM. Originals stay unchanged; history preserves references.
- Choose LiteLLM or Ollama. Discover installed Ollama image generators plus a source-linked verified catalog; selecting a missing model starts downloading it with progress.
- Store projects, prompt history, output indexes, video jobs and connection settings in SQLite. Existing JSON data is migrated automatically.
- Remember API keys in Windows Credential Manager, macOS Keychain or Linux Secret Service, and restore them on later launches. Keys are not stored as ordinary SQLite records.
- Resume video jobs from the in-app database list.

Ollama limitation: upstream v0.32.6 temporarily removed experimental image generation; v0.32.5 on macOS is the documented supported release. Discovery and downloads do not guarantee the connected server supports generation. Image editing and videos require LiteLLM. No exhaustive supported remote image/video catalog API was found; the app bundles verified official models and discovers additional installed image-capable models.

Windows: download and run `RustLiLm-Studio-windows-x64.exe`.
Linux: install the amd64 `.deb` with `sudo apt install ./rustlilm-studio-linux-amd64.deb`.
Linux targets Ubuntu 24.04 or newer compatible systems, requires a working Vulkan driver, and needs an unlocked Secret Service for key persistence.

Builds are unsigned. API workflows are tested with local mock servers; live provider generation requires your credentials/server.
