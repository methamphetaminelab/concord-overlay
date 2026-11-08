# concord-overlay

A Wayland voice overlay for [concord](https://github.com/chojs23/concord), the Discord TUI client: shows who is in your voice channel, like Discover.

- `patch/`: a concord patch that exports the voice channel state over a Unix socket (`protocol/PROTOCOL.md`).
- `overlay/`: a small Rust wlr-layer-shell client that reads the socket and draws a click-through overlay.
- `updater/`: a systemd user timer that builds the latest concord release with the patch, and the installer.

Install: `cd overlay && cargo build --release && cd .. && updater/install.sh`

Config: `~/.config/concord-overlay/config.toml`. Keys mirror Discover's voice overlay settings; see `overlay/config.example.toml`.

Requires a Wayland compositor with wlr-layer-shell (not GNOME).

License: GPL-3.0-or-later.
