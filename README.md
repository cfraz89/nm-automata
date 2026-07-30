# netman

Ergonomic NetworkManager bindings for Rust, generated from NetworkManager's own D-Bus spec.

```rust
use netman::{DeviceProxy, DeviceState, NetworkManagerProxy, ObjectPathSliceExt};

let connection = zbus::Connection::system().await?;
let nm = NetworkManagerProxy::new(&connection).await?;

let devices: Vec<DeviceProxy> = nm.get_devices().await?.proxies(&connection).await?;
for device in devices {
    if device.state().await? == DeviceState::Activated {
        println!("{} is up", device.interface().await?);
    }
}
```

`just example` prints every device, its type, state and capabilities.

## What is generated

Everything under `netman/src/generated/` — 53 proxies, 113 enumerations and flag sets, and
their documentation — comes from what NetworkManager publishes, pinned to the release in
`spec/nm-version.txt`:

| In `spec/` | Gives | How |
| --- | --- | --- |
| `introspection/*.xml` | the interfaces, and the gtk-doc documentation in their comments | fetched from the git tag |
| `NM-1.0.gir` | every enumeration and flag set, with values and documentation | built (`just build-spec`) |
| `nm-settings-docs-dbus.xml` | connection-profile properties and their D-Bus types | built (`just build-spec`) |

No C source is parsed. The first is fetched straight from the git tag and diffs against
upstream; the other two only exist once NetworkManager has been compiled, so a container does
that and the results are committed — see [How generation works](#how-generation-works).

Nothing is transcribed by hand. The only hand-written Rust in the crate is `src/repr.rs`
(two macros defining how a NetworkManager type is put on the wire) and `src/ext.rs` (three
traits for turning object paths into proxies), about 250 lines together.

## What that buys over raw proxies

* **Types instead of integers.** `device.state()` returns `DeviceState`, not `u32`;
  `device.capabilities()` returns a `bitflags` set. NetworkManager documents which properties
  and arguments are which, and generation reads that — then checks it against the D-Bus
  signature, so a documentation slip cannot produce a binding that fails to decode.
* **Forward compatibility.** Enumerations decode unknown values to `Other(n)` and flag sets
  keep unknown bits, so a crate built against one release still works against a newer daemon.
  The example above was generated from 1.58.0 and runs against 1.56.1.
* **Upstream's documentation, as rustdoc.** NetworkManager keeps its docs in XML comments,
  which plain introspection tooling drops. Generation lifts them back out and puts them on the
  proxies, their methods, and every argument.
* **Typed navigation.** Almost everything NetworkManager returns is an object path.
  `ObjectPathExt::proxy` and `ObjectPathSliceExt::proxies` turn those into proxies, and
  `ProxyExt::cast` moves between the several interfaces one object implements — a device object
  carries `Device`, `Device.Wired` and `Device.Statistics` at once.
* **Signals and property changes** come straight from zbus: `receive_state_changed()` for the
  property stream, `receive_state_changed_signal()` for the signal.

## Keeping up with upstream

Tasks are [`just`](https://just.systems) recipes; `just --list` shows them all.

```
just latest-version   # newest stable NetworkManager release
just fetch            # vendor the introspection XML (just fetch 1.56.1 to pin one)
just build-spec       # build the generated inputs in a container (slow, needs podman)
just generate         # regenerate netman/src/generated/
just update           # all three, in one step
just check            # fail if the checked-in output is stale
just regen            # generate, format, and run everything CI runs
```

`.github/workflows/update-spec.yml` runs `just update` weekly, verifies the result with
`just ci`, and opens a pull request. The diff under `netman/src/generated/` *is* the API
change, so a NetworkManager release is reviewed as a normal PR rather than chased by hand.
`.github/workflows/ci.yml` runs `just check` on every push, so the generator and its output
cannot drift apart.

## How generation works

`zbus_xmlgen` writes the proxies, but NetworkManager's introspection XML on its own would only
produce integers and undocumented methods. The generator in `codegen/` bridges that gap:

1. `codegen/src/gir.rs` reads `NM-1.0.gir`: every `<enumeration>` and `<bitfield>`, with values
   and documentation already resolved by the introspection scanner.
2. `codegen/src/annotate.rs` recovers the type references and documentation from the XML
   comments — `Returns: NMDeviceState`, `@managed: (NMDeviceManaged) …`.
3. `codegen/src/render.rs` writes both back into the XML as Telepathy `tp:` annotations, which
   `zbus_xmlgen` does understand, and emits the types as `nm_enum!`/`nm_flags!` invocations.
4. `codegen/src/codegen.rs` runs the generator per interface and assembles the result.

It stays a Rust crate rather than becoming shell recipes because it uses `zbus_xml` and
`zbus_xmlgen` as libraries; `just` is what invokes it.

### Why two of the three inputs are built rather than fetched

The types of enumerations and of connection-profile properties are not machine-readable
anywhere in NetworkManager's git tree — they come from GObject introspection data, which only
exists once libnm has been compiled. `just build-spec` compiles it at the pinned tag in a
container and runs NetworkManager's own generators. `ninja` is asked for one target, so the
daemon is never linked.

That cost lands only on whoever bumps the vendored release, which is already the moment a human
reviews a regeneration. The artifacts are committed, so building `netman` as a dependency needs
no container, no C toolchain and no GLib.

The settings documentation is *not* derived from the introspection data alone, even though it
could be: NetworkManager merges it with the `---dbus---` comment blocks in its sources, because
about thirty properties travel as something other than their GObject type — `ipv4.dns` is an
array of strings to GObject and an array of packed integers on the wire, and every MAC address
is a string to GObject and a byte array on D-Bus. Taking NetworkManager's merged answer avoids
having to reconcile that, and getting it wrong would corrupt profiles rather than fail loudly.

The augmented XML is written to `target/netman-augmented/` on every run, so what the generator
was fed is always inspectable.

Two details worth knowing:

* NetworkManager gives some interfaces a `State` property *and* a `StateChanged` signal, or a
  `Managed` property *and* a `SetManaged` method. Both want the same Rust name. The property
  keeps it; the other becomes `receive_state_changed_signal()` or `set_managed_method()`.
* Interfaces get one module each, because `Settings.Connection` and `VPN.Connection` both
  generate a `ConnectionProxy`. Names that are unique across the spec are re-exported at the
  crate root as they are; the rest keep their module's name in front —
  `SettingsConnectionProxy`, `VpnConnectionProxy`.

`annotations.toml` fills in the few members whose type upstream does not state in a form worth
inferring from — `Device.IPTunnel.Mode` is documented as "The tunneling mode." and nothing more.
Entries there are checked against the vendored spec, so one that stops applying fails generation
instead of rotting.

There are no signature tests against the spec. Everything on both sides of such a check comes
from the same vendored files and is regenerated together, so it would only ever restate what
generation just did; `just check` re-runs generation and diffs the result, which is the same
guard done properly.

## Features

`async-io` (default) or `tokio` picks the runtime zbus drives its connection with; enable
exactly one. `blocking-api` additionally generates the blocking proxies.

## Licence

MIT or Apache-2.0, at your option. The vendored spec under `spec/` is NetworkManager's, under
its own terms (LGPL-2.1-or-later).
