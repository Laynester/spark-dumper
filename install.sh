#!/usr/bin/env sh
set -e

# sparkd installer — fetches the prebuilt binary for this OS/arch from the
# latest GitHub release and installs it into ~/.local/bin (or /usr/local/bin
# when run as root). Usage: curl -sSL .../install.sh | sh

repo="Laynester/spark-dumper"
version="${SPARKD_VERSION:-latest}"

case "$(uname -s)" in
  Darwin) os="darwin" ;;
  Linux)  os="linux" ;;
  *)      echo "error: unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64|amd64)     arch="x86_64" ;;
  aarch64|arm64)    arch="arm64" ;;
  *)                echo "error: unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac

if [ "$version" = "latest" ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi
asset="sparkd-$version-$os-$arch.tar.gz"
url="$base/$asset"

if [ "$(id -u)" = "0" ]; then
  dest="/usr/local/bin"
else
  dest="$HOME/.local/bin"
fi
mkdir -p "$dest"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $url"
curl -sSL -o "$tmp/sparkd.tar.gz" "$url"
tar -xzf "$tmp/sparkd.tar.gz" -C "$tmp"
chmod +x "$tmp/sparkd/sparkd"
cp "$tmp/sparkd/sparkd" "$dest/sparkd"

echo "installed sparkd to $dest"
echo "run 'sparkd --help' to get started (add $dest to your PATH if needed)"