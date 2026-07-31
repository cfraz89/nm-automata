//! Ergonomic NetworkManager bindings for Rust, generated from NetworkManager's own D-Bus
//! spec.
//!
//! Every proxy, enumeration and flag set in this crate is produced by `just generate`
//! from the introspection XML and `nm-dbus-interface.h` of a pinned NetworkManager release —
//! see [`NM_VERSION`]. Nothing is transcribed by hand, so the API tracks upstream exactly,
//! including its documentation.
//!
//! # Getting started
//!
//! ```no_run
//! use nm_automata::{DeviceState, NetworkManagerProxy};
//!
//! # async fn example() -> zbus::Result<()> {
//! let connection = zbus::Connection::system().await?;
//! let nm = NetworkManagerProxy::new(&connection).await?;
//!
//! for device in nm.get_devices().await? {
//!     if device.state().await? == DeviceState::Activated {
//!         println!("{} is up", device.interface().await?);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # How it is put together
//!
//! * [`proxies`] holds one [`zbus::proxy`](macro@zbus::proxy) trait per D-Bus interface. The
//!   four singletons —
//!   [`NetworkManagerProxy`], [`SettingsProxy`], [`AgentManagerProxy`] and
//!   [`DnsManagerProxy`] — know their own object path, so `new(&connection)` is enough. Every
//!   other object is found at runtime.
//! * [`types`] holds NetworkManager's enumerations and flag sets. Properties and arguments
//!   that upstream documents as one of them are typed as such instead of as a bare integer.
//!   Both decode leniently, so a daemon newer than this binding still works: see
//!   [`DeviceState::Other`] and [`DeviceCapabilities::from_bits_retain`]. The connection-profile
//!   properties that travel as one of a fixed set of *strings* are there too —
//!   [`WirelessMode`], [`WirelessSecurityKeyMgmt`] — and decode the same way, keeping an
//!   unfamiliar value verbatim so that writing the profile back sends what was read.
//! * A few members are byte arrays that are not really bytes, or strings that are more than
//!   one thing, and have a type that says what they are: [`Ssid`] for the name of a Wi-Fi
//!   network — on [`WirelessSetting`] and on [`AccessPointProxy::ssid`] alike, since they are
//!   the same string — [`MacAddress`] for a hardware address, which NetworkManager writes as
//!   hex digits and colons everywhere except on the wire, and [`AssignedMacAddress`] for
//!   `assigned-mac-address`, which holds either an address or a word asking NetworkManager to
//!   work one out.
//! * Members that return object paths return the objects instead: `get_devices` hands back
//!   devices, built on the connection the proxy already holds. The paths are still there under
//!   a `_path` or `_paths` name — `get_device_paths` — and [`paths`] holds a newtype per
//!   interface so that even those say what they point at.
//! * [`ObjectPathExt`], [`ObjectPathSliceExt`] and [`ProxyExt`] turn bare object paths
//!   into typed proxies, and move between the several interfaces one
//!   object implements.
//!
//! # Signals and property changes
//!
//! These come straight from zbus. Each signal has a `receive_*` stream, and each property a
//! `receive_*_changed` one:
//!
//! ```no_run
//! use futures_util::StreamExt;
//! # use nm_automata::DeviceProxy;
//! # async fn example(device: DeviceProxy<'_>) -> zbus::Result<()> {
//! let mut changes = device.receive_state_changed().await;
//! while let Some(change) = changes.next().await {
//!     println!("now {:?}", change.get().await?);
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_debug_implementations)]

// A backstop only: zbus is a dependency and so is compiled first, and it has the same check,
// so in practice its message is the one seen. This one would fire if that ever went away.
#[cfg(not(any(feature = "async-io", feature = "tokio")))]
compile_error!(
    "nm-automata needs an async runtime for zbus to drive its connection with. Enable either \
     the `async-io` feature (on by default) or `tokio`."
);

mod empty;
mod ext;
mod generated;
mod mac;
mod path;
mod repr;
pub mod settings;
mod ssid;

pub use empty::NonEmpty;
pub use ext::{ObjectPathExt, ObjectPathSliceExt, ProxyExt};
pub use generated::devices::AnyDevice;
pub use generated::paths::*;
pub use generated::proxies::*;
pub use generated::settings as profile;
pub use generated::settings::*;
pub use generated::types::*;
pub use generated::vardict::*;
pub use generated::{NM_VERSION, devices, paths, proxies, types, vardict};
pub use mac::{AssignedMacAddress, InvalidMacAddress, MacAddress};
pub use path::{NmPath, NmPathSliceExt};
pub use ssid::Ssid;

/// zbus, re-exported: its [`Connection`](zbus::Connection) is the entry point to every proxy,
/// and its error type is what they return.
pub use zbus;
