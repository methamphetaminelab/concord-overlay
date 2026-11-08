# concord-overlay

A Wayland voice overlay for [concord](https://github.com/chojs23/concord), the Discord TUI client: shows who is in your voice channel, like Discover.

- `patch/`: a concord patch that exports the voice channel state over a Unix socket (`protocol/PROTOCOL.md`). The logic lives in `voice_export.rs`; `concord-hook.patch` only adds two lines.
- `overlay/`: a small Rust wlr-layer-shell client that reads the socket and draws a click-through overlay.

Build concord with the patch:

```
git clone https://github.com/chojs23/concord && cd concord && git checkout <release tag>
/path/to/concord-overlay/patch/apply.sh .
cargo build --release
```

Then run `target/release/concord` instead of the stock binary. For automated rebuilds on every concord release, see my-concord.

Install the overlay: `cd overlay && cargo build --release && cd .. && ./install.sh` (asks for the monitor, offers to disable Discover's autostart, enables `concord-overlay.service`).

Config: `~/.config/concord-overlay/config.toml`. Keys mirror Discover's voice overlay settings; see `overlay/config.example.toml`. The overlay reloads it on save and keeps the previous config if the new one is invalid.

Requires a Wayland compositor with wlr-layer-shell (not GNOME).

License: GPL-3.0-or-later.
