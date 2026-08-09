# nm-automata task runner. `just --list` shows everything.
#
# The generator lives in `codegen/` and stays Rust: it parses the introspection XML with
# `zbus_xml` and emits Rust with `syn` and `quote`, all of which it uses as libraries. These
# recipes are how it is invoked, and how the steps around it are composed.

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
# `origin` clones somewhere other than upstream — a fork, a mirror, or a local checkout, for a
# branch that need not be a release tag at all. A local directory is bind-mounted in, since the
# container otherwise has no access to the host filesystem to clone it from.
#
# Slow, and needs podman. It is only ever run when the vendored release changes: the result is
# committed, so building nm-automata as a dependency needs no container and no C toolchain.
build-spec version="" origin="":
    #!/usr/bin/env bash
    set -euo pipefail
    version="{{ version }}"
    [ -n "$version" ] || version="$(cat spec/nm-version.txt)"
    echo "building NetworkManager $version to generate its settings documentation"
    podman build -t nm-automata-spec -f spec/build/Containerfile spec/build/
    origin="{{ origin }}"
    mount=()
    if [ -n "$origin" ] && [ -d "$origin" ]; then
        mount=(-v "$(realpath "$origin"):/repo:Z,ro")
        origin=/repo
    fi
    podman run --rm "${mount[@]}" -v "$PWD/spec:/out:Z" nm-automata-spec "$version" "$origin"

# Regenerate `nm-automata/src/generated/` from the vendored spec.
generate:
    {{ codegen }} generate

# Vendor a spec and regenerate, in one step. Includes the container build, so it is slow.
update version="" origin="": (fetch version)
    #!/usr/bin/env bash
    set -euo pipefail
    just build-spec "$(cat spec/nm-version.txt)" "{{ origin }}"
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
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --package nm-automata {{ args }}

# Check against each supported runtime, since they are mutually exclusive.
#
# `check` rather than `build`: the question is whether each combination compiles, and asking it
# without linking is both faster and a great deal smaller on disk — three full builds of the
# generated code is most of what `target/` ever grows to.
features:
    cargo check --package nm-automata --no-default-features --features async-io
    cargo check --package nm-automata --no-default-features --features tokio
    cargo check --package nm-automata --no-default-features --features async-io,blocking-api
    # The examples too, since they are what the README points at for trying tokio.
    cargo check --package nm-automata --all-targets --no-default-features --features tokio

# Everything CI runs, in the order that fails fastest.
ci: check lint test doc features
    @just fmt --check

# Throw away this crate's build artifacts, keeping the compiled dependencies.
#
# That is where the bulk is — the generated code is large and gets built in several
# configurations — and recovering costs one crate's rebuild rather than the whole tree's. Use
# `cargo clean` for everything, when the disk really needs it.
clean:
    cargo clean --package nm-automata
    cargo clean --package codegen

# Talk to the NetworkManager on this machine.
example name="devices":
    cargo run --package nm-automata --example {{ name }}

# Regenerate, tidy up, and verify — what to run after changing the generator or the spec.
regen: generate fmt
    @just ci
