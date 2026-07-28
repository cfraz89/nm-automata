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

`cargo run --example devices` prints every device, its type, state and capabilities.

## What is generated

Everything under `netman/src/generated/` — 53 proxies, 80-odd enumerations and flag sets, and
their documentation — comes from two things NetworkManager publishes, pinned to the release in
`spec/nm-version.txt`:

| Vendored | Gives |
| --- | --- |
| `introspection/*.xml` | the interfaces, and the gtk-doc documentation in their comments |
| `nm-dbus-interface.h`, `nm-vpn-dbus-interface.h` | the enumerations and flag sets sent as integers |

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

```
cargo xtask latest-version   # newest stable NetworkManager release
cargo xtask fetch            # vendor its spec into spec/
cargo xtask generate         # regenerate netman/src/generated/
cargo xtask update           # both, in one step
cargo xtask check            # fail if the checked-in output is stale
```

`.github/workflows/update-spec.yml` runs `update` weekly, verifies the result builds and
passes its tests, and opens a pull request. The diff under `netman/src/generated/` *is* the
API change, so a NetworkManager release is reviewed as a normal PR rather than chased by hand.
`.github/workflows/ci.yml` runs `cargo xtask check` on every push, so the generator and its
output cannot drift apart.

## How generation works

`zbus_xmlgen` writes the proxies, but NetworkManager's introspection XML on its own would only
produce integers and undocumented methods. `cargo xtask generate` bridges that gap:

1. `xtask/src/header.rs` reads the C headers: every `typedef enum`, its values, its gtk-doc
   comment, and whether `/*< flags >*/` marks it as a bit set.
2. `xtask/src/annotate.rs` recovers the type references and documentation from the XML
   comments — `Returns: NMDeviceState`, `@managed: (NMDeviceManaged) …`.
3. `xtask/src/render.rs` writes both back into the XML as Telepathy `tp:` annotations, which
   `zbus_xmlgen` does understand, and emits the types as `nm_enum!`/`nm_flags!` invocations.
4. `xtask/src/codegen.rs` runs the generator per interface and assembles the result.

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
inferring from. Entries there are checked against the vendored spec, so one that stops applying
fails generation instead of rotting.

## Features

`async-io` (default) or `tokio` picks the runtime zbus drives its connection with; enable
exactly one. `blocking-api` additionally generates the blocking proxies.

## Licence

MIT or Apache-2.0, at your option. The vendored spec under `spec/` is NetworkManager's, under
its own terms (LGPL-2.1-or-later).
