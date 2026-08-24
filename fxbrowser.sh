#!/usr/bin/env bash
# Launcher for the FXBrowser release binary.
#
# On this machine (NVIDIA proprietary driver + Wayland), WebKitGTK fails to
# create a hardware GL context and renders a blank window unless software
# rendering is forced. If you're on a different GPU/driver and the window
# renders fine without this, you can drop these two exports.
export GDK_BACKEND=x11
export LIBGL_ALWAYS_SOFTWARE=1

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$dir/src-tauri/target/release/fxbrowser" "$@"
