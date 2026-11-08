#!/usr/bin/env bash
set -euo pipefail

dry=0
case ${1:-} in
    --dry-run) dry=1 ;;
    "") ;;
    *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac

root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bin=$HOME/.local/bin
units=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
cfgdir=${XDG_CONFIG_HOME:-$HOME/.config}/concord-overlay
autostart=${XDG_CONFIG_HOME:-$HOME/.config}/autostart

act() {
    if (( dry )); then printf '+'; printf ' %q' "$@"; echo; else "$@"; fi
}

if [[ ! -x $root/overlay/target/release/concord-overlay ]]; then
    echo "overlay not built: run 'cargo build --release' in $root/overlay, then re-run $0" >&2
    exit 1
fi

act mkdir -p "$bin" "$units"
act install -m 755 "$root/overlay/target/release/concord-overlay" "$bin/concord-overlay"
act install -m 644 "$root/overlay/concord-overlay.service" "$units/"

if [[ ! -e $cfgdir/config.toml ]]; then
    echo "Connected outputs:"
    for s in /sys/class/drm/*/status; do
        [[ -r $s && $(cat "$s") == connected ]] || continue
        n=${s%/status}; n=${n##*/}; echo "  ${n#card*-}"
    done
    if (( dry )); then mon=HDMI-A-1; else
        read -r -p "Monitor name [HDMI-A-1]: " mon; mon=${mon:-HDMI-A-1}
    fi
    if [[ ! $mon =~ ^[A-Za-z0-9._-]+$ ]]; then echo "bad monitor name: $mon" >&2; exit 1; fi
    act mkdir -p "$cfgdir"
    if (( dry )); then
        echo "+ write $cfgdir/config.toml from overlay/config.example.toml with monitor = \"$mon\""
    else
        sed "s|^monitor = \"Any\"|monitor = \"$mon\"|" "$root/overlay/config.example.toml" > "$cfgdir/config.toml"
    fi
fi

orig=$autostart/discover_overlay.desktop
if [[ -f $orig ]] && ! grep -qx 'Hidden=true' "$orig"; then
    ans=n
    if (( dry )); then echo "(would ask: disable Discover autostart? [y/N])"; ans=y
    else read -r -p "Disable Discover autostart? [y/N] " ans; fi
    if [[ $ans == [yY] ]]; then
        if [[ -L $orig || ! -w $orig ]]; then act mv -f "$orig" "$orig.bak"
        else act cp -n "$orig" "$orig.bak"; fi
        if (( dry )); then echo "+ append Hidden=true to $orig"
        else sed '/^Hidden=/d; /^\[Desktop Entry\]/a Hidden=true' "$orig.bak" > "$orig"; fi
    fi
fi

act systemctl --user daemon-reload
act systemctl --user enable --now concord-overlay.service

echo "If Discover's overlay is running now, close it (or log out and back in) so only one overlay shows."
