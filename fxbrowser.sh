#!/usr/bin/env bash
# Launcher for the FXBrowser release binary.
#
# On this machine (NVIDIA proprietary driver + Wayland), WebKitGTK fails to
# create a hardware GL context and renders a blank window unless software
# rendering is forced. If you're on a different GPU/driver and the window
# renders fine without this, you can drop these exports.
export GDK_BACKEND=x11
export LIBGL_ALWAYS_SOFTWARE=1
# WebKitGTK >= 2.42 also uses a DMA-BUF renderer for accelerated compositing
# that allocates GBM buffers independently of the GL backend above. On NVIDIA
# this fails ("Failed to create GBM buffer ...") and leaves a gray window, so
# disable it and fall back to plain software compositing.
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$dir/src-tauri/target/release/fxbrowser"

# Tauri embeds dist/ and the Rust sources into the binary at build time, so
# editing them alone doesn't change what's running. Rebuild automatically
# whenever any of that is newer than the binary we're about to launch.
if [ ! -x "$bin" ] || [ -n "$(find "$dir/dist" "$dir/src-tauri/src" "$dir/src-tauri/Cargo.toml" "$dir/src-tauri/tauri.conf.json" -newer "$bin" -type f 2>/dev/null)" ]; then
    echo "fxbrowser.sh: source is newer than the built binary, rebuilding..." >&2
    # build.rs only declares cargo:rerun-if-changed on tauri.conf.json and
    # capabilities/ — NOT on dist/ — so cargo won't notice dist-only edits
    # and would silently keep the old embedded frontend. Touch build.rs so
    # cargo always reruns it and re-embeds whatever's in dist/ right now.
    touch "$dir/src-tauri/build.rs"
    (cd "$dir/src-tauri" && cargo build --release)
fi

exec "$bin" "$@"
