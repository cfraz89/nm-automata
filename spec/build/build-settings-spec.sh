#!/bin/bash
# Produce the two generated inputs nm-automata needs, for a NetworkManager release tag, into /out:
#
#   NM-1.0.gir                 every enumeration and flag set, with values and documentation
#   nm-settings-docs-dbus.xml  connection-profile properties, with their D-Bus types
#
# The second is not simply derived from the first: NetworkManager merges the introspection data
# with the `---dbus---` comment blocks in its sources, because roughly thirty properties travel
# as something other than their GObject type (a MAC address is a string to GObject and a byte
# array on the wire). Taking NetworkManager's merged answer avoids having to reconcile that.
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

# The introspection data is a dependency of the settings documentation, so one target builds
# both of the artifacts wanted here.
ninja -C build "$target"

gir=$(find build -name 'NM-1.0.gir' -print -quit)
[ -n "$gir" ] || { echo "NM-1.0.gir was not produced" >&2; exit 1; }

install -m 0644 "$gir" /out/NM-1.0.gir
install -m 0644 "build/$target" /out/nm-settings-docs-dbus.xml
echo "wrote NM-1.0.gir and nm-settings-docs-dbus.xml for NetworkManager $version"
