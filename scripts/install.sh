#!/bin/sh
# Install the latest spandeck release for this machine.
#   curl -fsSL https://raw.githubusercontent.com/michaelbaker-dev/spandeck/main/scripts/install.sh | sh
# Options (environment): SPANDECK_VERSION=v0.1.0  SPANDECK_BIN_DIR=$HOME/.local/bin
#   SPANDECK_RELEASES=<address of a copy of the release page>
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

version=${SPANDECK_VERSION:-}
if [ -z "$version" ]; then
  version=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest" | sed 's#.*/tag/##')
fi
case "$version" in v*) ;; *) echo "could not find the latest release of $repo" >&2; exit 1 ;; esac

name=spandeck-$version-$target
url=$releases/download/$version/$name.tar.gz
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $name"
curl -fsSL -o "$tmp/$name.tar.gz" "$url"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$url.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null)
else
  (cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null)
fi
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
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
