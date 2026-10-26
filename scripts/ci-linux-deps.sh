#!/usr/bin/env bash
set -euo pipefail
# The hosted image is mutable even with ubuntu-24.04. Record what was present
# and avoid upgrading already installed packages just to obtain headers.
packages=(ffmpeg libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev)
missing=()
for package in "${packages[@]}"; do
  if ! dpkg-query -W -f='${Status}\n' "$package" 2>/dev/null | grep -qx 'install ok installed'; then
    missing+=("$package")
  fi
done
dpkg-query -W "${packages[@]}" || true
if ((${#missing[@]})); then
  options=(-o Acquire::Retries=1 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20 -o DPkg::Lock::Timeout=30)
  # A trickling mirror need not trigger socket timeouts: bound total wall time.
  timeout --kill-after=10s 90s sudo apt-get "${options[@]}" update
  timeout --kill-after=10s 180s sudo apt-get "${options[@]}" install -y --no-install-recommends "${missing[@]}"
fi
ffmpeg -version | head -n 1
ffprobe -version | head -n 1
pkg-config --modversion gtk+-3.0 webkit2gtk-4.1
