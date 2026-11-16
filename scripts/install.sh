#!/bin/sh
# Install the latest spandeck release for this machine.
#   curl -fsSL https://raw.githubusercontent.com/michaelbaker-dev/spandeck/main/scripts/install.sh | sh
# Options (environment): SPANDECK_VERSION=v0.1.0  SPANDECK_BIN_DIR=$HOME/.local/bin
#   SPANDECK_RELEASES=<address of a copy of the release page>
#   SPANDECK_ARCHIVE=<a release archive already on this machine>, with SPANDECK_VERSION
#     and SPANDECK_ARCHIVE_SHA256=<its SHA-256>: nothing is downloaded
# Ends with 75 when the release could not be downloaded, 1 for anything else.
set -eu
repo=michaelbaker-dev/spandeck
bin_dir=${SPANDECK_BIN_DIR:-$HOME/.local/bin}
releases=${SPANDECK_RELEASES:-https://github.com/$repo/releases}

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) echo "spandeck runs on macOS and Linux only" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) echo "unsupported processor: $(uname -m)" >&2; exit 1 ;;
esac
target=$arch-$os

# The release could not be had, as against: it was had and could not be installed.
no_download() { echo "$1" >&2; exit 75; }
sum_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1
}

archive=${SPANDECK_ARCHIVE:-}
version=${SPANDECK_VERSION:-}
if [ -n "$archive" ]; then
  want=${SPANDECK_ARCHIVE_SHA256:-}
  if [ -z "$version" ] || [ ! -f "$archive" ] || [ "${#want}" != 64 ]; then
    echo "SPANDECK_ARCHIVE needs the file, SPANDECK_VERSION and SPANDECK_ARCHIVE_SHA256" >&2
    exit 1
  fi
elif [ -z "$version" ]; then
  version=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest" | sed 's#.*/tag/##') || version=
  case "$version" in v*) ;; *) no_download "could not find the latest release of $repo" ;; esac
fi
case "$version" in v*) ;; *) echo "$version is not a release name" >&2; exit 1 ;; esac

name=spandeck-$version-$target
url=$releases/download/$version/$name.tar.gz
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if [ -n "$archive" ]; then
  echo "Installing $name from $archive"
  if [ "$(sum_of "$archive")" != "$want" ]; then
    echo "$archive does not have the SHA-256 given for it. Nothing was installed." >&2
    exit 1
  fi
else
  echo "Downloading $name"
  archive=$tmp/$name.tar.gz
  curl -fsSL -o "$archive" "$url" || no_download "The download of $name did not finish."
  curl -fsSL -o "$archive.sha256" "$url.sha256" || no_download "The checksum of $name could not be downloaded."
  want=$(cut -d ' ' -f 1 < "$archive.sha256")
  if [ "${#want}" != 64 ] || [ "$(sum_of "$archive")" != "$want" ]; then
    no_download "The download does not match its published checksum. Nothing was installed."
  fi
fi
tar -xzf "$archive" -C "$tmp"
mkdir -p "$bin_dir"
# Install beside, then rename, so a running daemon keeps its old file until upgraded.
for b in spandeck sd; do
  cp "$tmp/$name/$b" "$bin_dir/.$b.new"
  chmod 755 "$bin_dir/.$b.new"
  mv "$bin_dir/.$b.new" "$bin_dir/$b"
done
echo "Installed spandeck $version to $bin_dir (checksum verified)"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add it to your PATH:  export PATH=\"$bin_dir:\$PATH\"" ;;
esac
echo "Next:  spandeck service --install   then   spandeck doctor"
