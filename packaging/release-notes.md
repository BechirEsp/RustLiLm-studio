Native Rust + GPUI studio for LiteLLM image and video generation.

- Connect to a gateway and discover the models available to your API key.
- Select an output folder, optional project folders, and daily grouping.
- Generate images, submit/poll video jobs, and resume saved video jobs.
- Browse persistent local results; hover to copy an image or reveal its file.
- Play videos in your operating system's default player.

Windows: download and run `RustLiLm-Studio-windows-x64.exe`.
Linux: install the amd64 `.deb` with `sudo apt install ./rustlilm-studio-linux-amd64.deb`.
The Linux package targets Ubuntu 24.04 or newer compatible systems and requires a working Vulkan driver.

These builds are unsigned. Generation requires a LiteLLM gateway exposing the image/video endpoints and a permitted model. End-to-end requests were tested against a mock gateway; live provider generation requires your gateway credentials.
