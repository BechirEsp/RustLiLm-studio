RustLiLm Studio v0.2.0 adds a project-based workspace and in-app image viewing.

- Click an image to open it inside the app. Close it with Close or Escape.
- Copy an image from the gallery or viewer and receive an Image copied toast.
- Create/select projects in the left sidebar, with a project count and expandable prompt histories.
- Select a previous prompt to restore its settings and view its outputs.
- Use the bottom composer: prompt on the left; output type, model, size, and Generate on the right.
- Open Settings over the studio. Save applies changes; Close, Cancel, and Escape discard unsaved edits and return home.
- All JSON metadata and video job checkpoints now live in the shared `_studio` folder. Existing v0.1 metadata is migrated automatically; media files stay in place.

Windows: download and run `RustLiLm-Studio-windows-x64.exe`.
Linux: install the amd64 `.deb` with `sudo apt install ./rustlilm-studio-linux-amd64.deb`.
The Linux package targets Ubuntu 24.04 or newer compatible systems and requires a working Vulkan driver.

Builds are unsigned. Gateway integration is tested with a local mock server; live provider generation requires your gateway credentials.
