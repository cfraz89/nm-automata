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
//! use netman::{DeviceState, NetworkManagerProxy};
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
//!   [`DeviceState::Other`] and [`DeviceCapabilities::from_bits_retain`].
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
//! # use netman::DeviceProxy;
//! # async fn example(device: DeviceProxy<'_>) -> zbus::Result<()> {
//! let mut changes = device.receive_state_changed().await;
//! while let Some(change) = changes.next().await {
//!     println!("now {:?}", change.get().await?);
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_debug_implementations)]

mod ext;
mod generated;
mod path;
mod repr;
pub mod settings;

pub use ext::{ObjectPathExt, ObjectPathSliceExt, ProxyExt};
pub use generated::devices::AnyDevice;
pub use generated::paths::*;
pub use generated::proxies::*;
pub use generated::settings as profile;
pub use generated::settings::*;
pub use generated::types::*;
pub use generated::{NM_VERSION, devices, paths, proxies, types};
pub use path::{NmPath, NmPathSliceExt};

/// zbus, re-exported: its [`Connection`](zbus::Connection) is the entry point to every proxy,
/// and its error type is what they return.
pub use zbus;
