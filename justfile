# netman task runner. `just --list` shows everything.
#
# The generator lives in `codegen/` and stays Rust: it drives `zbus_xmlgen` and parses the
# introspection XML with `zbus_xml`, both of which it uses as libraries. These recipes are
# how it is invoked, and how the steps around it are composed.

set shell := ["bash", "-euo", "pipefail", "-c"]

# The generator, quiet enough to be piped.
codegen := "cargo run --quiet --package codegen --"

# Show the available recipes.
default:
    @just --list

# Pass a tag to pin a release — `just fetch 1.56.1` — or nothing for the newest stable.
# Vendor the D-Bus spec of a NetworkManager release into `spec/`.
fetch version="":
    {{ codegen }} fetch {{ if version == "" { "" } else { "--version " + version } }}

# Rebuild `spec/nm-settings-docs-dbus.xml` by compiling NetworkManager in a container.
#
# The types of connection-profile properties are derived from GObject introspection data, which
# only exists once libnm has been compiled — there is nothing machine-readable in the git tree
# to read them from. This runs NetworkManager's own generator against a release tag.
#
# Slow, and needs podman. It is only ever run when the vendored release changes: the result is
# committed, so building netman as a dependency needs no container and no C toolchain.
build-spec version="":
    #!/usr/bin/env bash
    set -euo pipefail
    version="{{ version }}"
    [ -n "$version" ] || version="$(cat spec/nm-version.txt)"
    echo "building NetworkManager $version to generate its settings documentation"
    podman build -t netman-nmspec -f spec/build/Containerfile spec/build/
    podman run --rm -v "$PWD/spec:/out:Z" netman-nmspec "$version"

# Regenerate `netman/src/generated/` from the vendored spec.
generate:
    {{ codegen }} generate

# Vendor a spec and regenerate, in one step. Includes the container build, so it is slow.
update version="": (fetch version)
    #!/usr/bin/env bash
    set -euo pipefail
    just build-spec "$(cat spec/nm-version.txt)"
    just generate

# Print the newest upstream NetworkManager stable release tag.
latest-version:
    @{{ codegen }} latest-version

# The NetworkManager release the checked-in bindings were generated from.
vendored-version:
    @cat spec/nm-version.txt

# Fail if the checked-in generated code is stale.
check:
    {{ codegen }} check

# `just fmt --check` verifies instead of rewriting.
fmt *args:
    cargo fmt --all {{ args }}

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# The doctests double as compile checks on the generated proxies.
test:
    cargo test --workspace

doc *args:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --package netman {{ args }}

# Build against each supported runtime, since they are mutually exclusive.
features:
    cargo build --package netman --no-default-features --features async-io
    cargo build --package netman --no-default-features --features tokio
    cargo build --package netman --no-default-features --features async-io,blocking-api

# Everything CI runs, in the order that fails fastest.
ci: check lint test doc features
    @just fmt --check

# Talk to the NetworkManager on this machine.
example name="devices":
    cargo run --package netman --example {{ name }}

# Regenerate, tidy up, and verify — what to run after changing the generator or the spec.
regen: generate fmt
    @just ci
