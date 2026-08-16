# Keybindings for Sidecar Overlay Control

Use the `sidecar` CLI with your compositor's keybinding configuration to control the overlay without touching the Sidecar UI.

## Prerequisites

The `sidecar` binary must be in your `$PATH`. If you built from source:

```sh
cargo build --release -p sidecar-cli
cp target/release/sidecar ~/.local/bin/
```

## Hyprland

Add to `~/.config/hypr/hyprland.conf`:

```conf
bind = SUPER, D, exec, sidecar draw
bind = SUPER SHIFT, C, exec, sidecar clear
```

## Sway

Add to `~/.config/sway/config`:

```conf
set $mod Mod4
bindsym $mod+d exec sidecar draw
bindsym $mod+Shift+c exec sidecar clear
```

## Available Commands

| Command                      | Description                                    |
| ---------------------------- | ---------------------------------------------- |
| `sidecar host`               | Start a screen sharing session (TUI dashboard) |
| `sidecar join <code>`        | Open the web viewer in your browser            |
| `sidecar draw`               | Toggle draw mode on/off                        |
| `sidecar draw on`            | Enable draw mode                               |
| `sidecar draw off`           | Disable draw mode                              |
| `sidecar clear`              | Clear all annotations                          |
| `sidecar set-resolution <h>` | Set resolution override (0 = clear)            |

## Notes

- Overlay control commands are fire-and-forget: they connect to the overlay socket, send the command, and exit.
- Success is silent (exit code 0). If no Sidecar session is running, the CLI prints an error to stderr and exits with code 1.
- The suggested key combinations (Super+D, Super+Shift+C) are recommendations. Use whatever bindings work for your workflow.
- If `sidecar` is not in your `$PATH`, use the full path in the keybinding config (e.g., `exec /home/you/.local/bin/sidecar draw`).
