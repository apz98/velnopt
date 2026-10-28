#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

cargo build -p velnopt-cli --release --locked
read -r _ velnopt_version < <(target/release/velnopt --version)
velnopt_bundle="velnopt-${velnopt_version}-linux-$(uname -m)"
velnopt_staging="$(mktemp -d)"
trap 'rm -rf -- "$velnopt_staging"' EXIT

install -Dm755 target/release/velnopt "$velnopt_staging/$velnopt_bundle/bin/velnopt"
install -Dm644 examples/hyprland.toml "$velnopt_staging/$velnopt_bundle/share/velnopt/panel.toml"
install -Dm644 examples/live-hyprland.toml "$velnopt_staging/$velnopt_bundle/share/velnopt/live-hyprland.toml"
install -Dm644 examples/desktop.toml "$velnopt_staging/$velnopt_bundle/share/velnopt/desktop.toml"
for velnopt_document in LICENSE README.md CHANGELOG.md CONTRIBUTING.md SECURITY.md; do
    install -Dm644 "$velnopt_document" "$velnopt_staging/$velnopt_bundle/share/doc/velnopt/$velnopt_document"
done
for velnopt_document in docs/*.md docs/decisions/*.md examples/*.toml; do
    install -Dm644 "$velnopt_document" "$velnopt_staging/$velnopt_bundle/share/doc/velnopt/$velnopt_document"
done
python3 scripts/collect-licenses.py "$velnopt_staging/$velnopt_bundle/share/doc/velnopt/third-party"
mkdir -p target/dist
tar -czf "target/dist/$velnopt_bundle.tar.gz" -C "$velnopt_staging" "$velnopt_bundle"
(
    cd target/dist
    sha256sum "$velnopt_bundle.tar.gz" > "$velnopt_bundle.tar.gz.sha256"
)
printf 'Package: target/dist/%s.tar.gz\n' "$velnopt_bundle"
