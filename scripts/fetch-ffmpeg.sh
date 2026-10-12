#!/usr/bin/env bash
# Download static ffmpeg + ffprobe for a target and stage them for bundling.
#
#   scripts/fetch-ffmpeg.sh [target-triple] [out-dir]
#
# Writes strikecut-ffmpeg-<triple>[.exe] and strikecut-ffprobe-<triple>[.exe], the names Tauri's `externalBin`
# expects (it strips the triple when bundling). The "strikecut-" prefix keeps them from colliding with a system
# ffmpeg, e.g. in /usr/bin when installed from the .deb. Every download is checked against a pinned SHA-256; when
# a source publishes a new build, update its URL and hash here.
set -euo pipefail

target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
out="${2:-$(cd "$(dirname "$0")/.." && pwd)/app/src-tauri/binaries}"

btbn=https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08
ext=""
case "$target" in
  x86_64-pc-windows-msvc)
    ext=.exe
    # Gyan's "essentials" build: GPLv3, includes libx264, and much smaller than a full build.
    archives=("https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip 60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba")
    source="https://www.gyan.dev/ffmpeg/builds/ (9.0.2 essentials)"
    ;;
  x86_64-unknown-linux-gnu)
    archives=("$btbn/ffmpeg-n9.0.2-17-g2a571b6068-linux64-gpl-9.0.tar.xz 68ee646831adaae2495618346f3bba94ff207ff83bbd34d643e7004730d66269")
    source="https://github.com/BtbN/FFmpeg-Builds (n9.0.2, linux64-gpl)"
    ;;
  aarch64-unknown-linux-gnu)
    archives=("$btbn/ffmpeg-n9.0.2-17-g2a571b6068-linuxarm64-gpl-9.0.tar.xz 91afcd7695d4bbbb9e0057aa10baaf9875ec4e1a97303f9059ec971336a2ab46")
    source="https://github.com/BtbN/FFmpeg-Builds (n9.0.2, linuxarm64-gpl)"
    ;;
  aarch64-apple-darwin)
    archives=(
      "https://www.osxexperts.net/ffmpeg9arm.zip d0c06c5c68ce48af3143b262f7a9118a7c9f67de1e237fcc24ffb14df9c67af9"
      "https://www.osxexperts.net/ffprobe9arm.zip 0c94fbdd8917022f28115eca512196cf4648732bc9e5db9ec8896c7e519d02aa"
    )
    source="https://www.osxexperts.net (9.0, Apple Silicon)"
    ;;
  x86_64-apple-darwin)
    archives=(
      "https://www.osxexperts.net/ffmpeg80intel.zip 2d24d22db78c87f394a5822867acd5c5dc5e762cd261a44bd26923f3a5af3e07"
      "https://www.osxexperts.net/ffprobe80intel.zip 0b6576104a95c1b39d4939e2df86f8f7cf1d55287ff57da48777d94605d12feb"
    )
    source="https://www.osxexperts.net (8.0, Intel)"
    ;;
  *)
    echo "no prebuilt ffmpeg configured for $target" >&2
    exit 1
    ;;
esac

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

for entry in "${archives[@]}"; do
  read -r url hash <<<"$entry"
  file="$work/$(basename "$url")"
  echo "Downloading $url"
  curl -fsSL --retry 3 -o "$file" "$url"
  actual="$(sha256 "$file")"
  if [ "$actual" != "$hash" ]; then
    echo "checksum mismatch for $url: expected $hash, got $actual" >&2
    exit 1
  fi
  case "$file" in
    *.zip) unzip -q -o "$file" -d "$work/x" ;;
    *.tar.xz) mkdir -p "$work/x" && tar -xJf "$file" -C "$work/x" ;;
  esac
done

for tool in ffmpeg ffprobe; do
  found="$(find "$work/x" -type f -name "$tool$ext" -not -path '*__MACOSX*' | head -n 1)"
  if [ -z "$found" ]; then
    echo "$tool$ext not found in the downloaded archive" >&2
    exit 1
  fi
  dest="$out/strikecut-$tool-$target$ext"
  cp "$found" "$dest"
  chmod +x "$dest"
  echo "Wrote $dest"
done

cat >"$out/FFMPEG-NOTICE.txt" <<EOF
The strikecut-ffmpeg and strikecut-ffprobe programs bundled with Strikecut are unmodified builds of FFmpeg
(https://ffmpeg.org), licensed under the GNU General Public License. They run as separate programs; Strikecut
itself is MIT licensed.

Build: $source
Source code: https://ffmpeg.org/download.html
License: https://ffmpeg.org/legal.html
EOF
