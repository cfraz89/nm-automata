#!/bin/bash
# Produce nm-settings-docs-dbus.xml for a NetworkManager release tag into /out.
set -euo pipefail

version="${1:?usage: build-settings-spec <nm-version-tag>}"
target="man/nm-settings-docs-dbus.xml"

git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$version" \
    https://gitlab.freedesktop.org/NetworkManager/NetworkManager.git /src
cd /src

# Only the generator's own dependency chain gets built: the introspection data, and the libnm
# objects it is scanned from. The daemon itself is never linked.
# Introspection and docs are the only features that matter here. Daemon-side ones are turned
# off because they pull build dependencies that have nothing to do with generating the
# settings documentation — CLAT needs libbpf, and so on.
meson setup build \
    -Dintrospection=true \
    -Ddocs=true \
    -Dman=false \
    -Dtests=no \
    -Dclat=false \
    -Dcrypto=null

ninja -C build "$target"

install -m 0644 "build/$target" /out/nm-settings-docs-dbus.xml
echo "wrote nm-settings-docs-dbus.xml for NetworkManager $version"
