<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/sidecar-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="brand/sidecar-lockup.svg">
  <img alt="Sidecar" src="brand/sidecar-lockup.svg" height="80">
</picture>

<br>

Screen sharing that feels like sharing a desk: your pair sees your screen live, draws right on it, and takes the keyboard when you say so.

Sidecar streams your desktop over WebRTC. Direct peer-to-peer where possible, with TURN relay fallback. Inviting your pair is one sentence: "go to [view.sidecar.rs](https://view.sidecar.rs) and type `brave-parrot-42`" (or just send them the link). Their annotations land directly on your desktop as a transparent overlay while you keep typing, and when pointing isn't enough, they grab the mouse and keyboard and drive.

## Features

- **Peer-to-peer**: Direct WebRTC connections where possible, with TURN relay fallback
- **Hardware-accelerated encoding**: Prefers AV1 (VideoToolbox / VAAPI), falls back to H.264 or software VP9
- **Adaptive bitrate**: Degrades FPS -> quality -> resolution in that order based on network conditions
- **Bi-directional annotations**: Both host and viewers draw; strokes appear on all screens in real-time and fade after ~2 seconds (Shift+click to persist)
- **Remote control**: Viewers drive the host's mouse and keyboard on demand (Linux/Wayland, via the XDG RemoteDesktop portal or wlroots virtual-input protocols). The host gates it with a session-wide switch; one viewer drives at a time and another takes over by clicking ("tap to take control"), while every viewer's pointer shows color-coded to everyone
- **Draw mode**: Host toggles between drawing on the overlay and clicking through to their editor (`Ctrl+Shift+D`)
- **Wayland overlay**: Host annotations and remote-control cursors render on a Wayland layer-shell surface (Linux); viewers also render them client-side from data-channel packets for low latency
- **Up to 3 concurrent viewers** per session: each viewer gets its own encoded track, so people on different browsers (e.g. Chrome and Firefox) can share a session and a slow viewer never stalls the others
- **Terminal UI**: `sidecar-cli` lets you host a session from the terminal without the desktop app (Linux)

## Getting Started

Only the host installs anything. Download the latest release for your platform from [GitHub Releases](https://github.com/sidecar-app/sidecar/releases/latest):

- **Linux**: AppImage
- **macOS**: DMG (Apple Silicon)
- **Windows**: NSIS installer
- **Linux CLI**: Standalone `sidecar` binary with built-in overlay (no GUI dependencies)

Prefer to compile it yourself? See [Build from Source](#build-from-source).

### Your First Session

1. **Host**: Click **Start session**. A room code appears (e.g. `brave-parrot-42`). Read it out to your pair partner, or press `Cmd/Ctrl+C` to copy a ready-to-share join link.
2. **Viewer**: Open [view.sidecar.rs](https://view.sidecar.rs) (or the link the host sent you), enter the room code, and click **Join**.
3. **Draw mode**: Press `Ctrl+Shift+D` to toggle between drawing on the overlay and clicking through to your editor.
4. **Remote control**: The host enables it with the "Allow control" button (`Ctrl+Shift+R` in the desktop app) or the `r` key (CLI). The first time a viewer requests control, the host is prompted to allow it for the session (which lets everyone in the session control) or deny. A viewer presses `R` to enter remote-control mode; one viewer drives at a time and clicking takes over control. The host's pointer follows your clicks and drags (plain hovering just shows your cursor, so it never fights the host's own mouse). `Esc` stops controlling.

### Keyboard Shortcuts

**Host**

| Key            | Action                |
| -------------- | --------------------- |
| `Ctrl+Shift+D` | Toggle draw mode      |
| `Ctrl+Shift+R` | Toggle remote control |
| `Cmd/Ctrl+C`   | Copy room link        |
| `Escape`       | Stop session          |

**Viewer**

| Key                 | Action                                  |
| ------------------- | --------------------------------------- |
| `D`                 | Toggle draw mode                        |
| `R`                 | Toggle remote control mode              |
| `S`                 | Toggle stats overlay                    |
| `Shift+Click`       | Draw persistent annotation (won't fade) |
| `+` / `-`           | Zoom in / out                           |
| Arrow keys / `hjkl` | Pan (when zoomed)                       |
| `Space+drag`        | Pan                                     |
| Double-click        | Reset zoom                              |
| `Escape`            | Leave session                           |

### CLI (Linux)

```sh
sidecar                         # Start TUI dashboard
sidecar join brave-parrot-42    # Open viewer in browser
sidecar draw                    # Toggle draw mode on/off
sidecar draw on                 # Enable overlay draw mode
sidecar draw off                # Disable overlay draw mode
sidecar clear                   # Clear all annotations
```

## Platforms

|                     | Screen capture           | Hardware encode                  | Overlay             | Remote control | Desktop app | CLI |
| ------------------- | ------------------------ | -------------------------------- | ------------------- | -------------- | ----------- | --- |
| **Linux** (Wayland) | PipeWire via XDG Portal  | VAAPI (VP9, AV1)                 | Wayland layer-shell | Yes            | Yes         | Yes |
| **macOS** (13+)     | ScreenCaptureKit         | VideoToolbox (H.264, AV1 on M4+) | Yes                 | —              | Yes         | —   |
| **Windows** (10+)   | Windows.Graphics.Capture | —                                | Yes                 | —              | Yes         | —   |

Software VP9 encoding (libvpx) is available on all platforms, used as a fallback on Linux/macOS when no hardware encoder is detected, and as the only encoder on Windows for now.

## Architecture

```
Screen Capture --> Encode --> WebRTC --> Viewer (browser)
   (capture)       (encode)   (net)       (viewer-web)
                                ▲
                        signaling server
```

| Crate             | Purpose                                                               |
| ----------------- | --------------------------------------------------------------------- |
| `capture`         | Platform-native screen capture (PipeWire, ScreenCaptureKit, WGC)      |
| `encode`          | VP9/H.264/AV1 encoding, colorspace conversion, adaptive bitrate       |
| `net`             | WebRTC peer connections, signaling client, stats collection           |
| `pipeline`        | Orchestrates capture -> encode -> network pipeline                    |
| `input`           | Synthetic mouse/keyboard injection for remote control (Linux)         |
| `ipc`             | Message protocol between host app and overlay process                 |
| `overlay-sidecar` | Wayland layer-shell overlay library for host-side annotations (Linux) |
| `sidecar-cli`     | Terminal UI for hosting sessions without the desktop app (Linux)      |

## Build from Source

**All platforms** need [Rust](https://rustup.rs/) and [Deno](https://deno.com/).

<details>
<summary><b>Linux</b></summary>

```sh
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf \
  libvpx-dev libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev libclang-dev \
  libavutil-dev libavcodec-dev libavformat-dev libavfilter-dev \
  libavdevice-dev libswscale-dev libswresample-dev
```

</details>

<details>
<summary><b>macOS</b></summary>

```sh
brew install libvpx ffmpeg
```

</details>

<details>
<summary><b>Windows</b></summary>

Requires Windows 10 1903+ (for the screen capture API), Visual Studio 2022 Build Tools with the C++ workload, [LLVM](https://releases.llvm.org/) (bindgen needs `libclang.dll`), and [vcpkg](https://github.com/microsoft/vcpkg) for libvpx:

```powershell
vcpkg install libvpx:x64-windows-static
setx LIBCLANG_PATH "C:\Program Files\LLVM\bin"
setx VPX_LIB_DIR "C:\vcpkg\installed\x64-windows-static\lib"
setx VPX_INCLUDE_DIR "C:\vcpkg\installed\x64-windows-static\include"
```

Open a new terminal so the persisted env vars take effect before building.

</details>

Then clone, install, and run:

```sh
git clone https://github.com/sidecar-app/sidecar.git
cd sidecar
deno install
cd host-app
deno task tauri dev
```

To build the CLI instead (Linux only):

```sh
cargo build --release -p sidecar-cli
```

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.

## License

[MIT](LICENSE)
