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
//! use netman::{DeviceProxy, DeviceState, NetworkManagerProxy, ObjectPathSliceExt};
//!
//! # async fn example() -> zbus::Result<()> {
//! let connection = zbus::Connection::system().await?;
//! let nm = NetworkManagerProxy::new(&connection).await?;
//!
//! let devices: Vec<DeviceProxy> = nm.get_devices().await?.proxies(&connection).await?;
//! for device in devices {
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
//! * [`ObjectPathExt`], [`ObjectPathSliceExt`] and [`ProxyExt`] turn the object paths
//!   NetworkManager hands out into typed proxies, and move between the several interfaces one
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
mod repr;
pub mod settings;

pub use ext::{ObjectPathExt, ObjectPathSliceExt, ProxyExt};
pub use generated::proxies::*;
pub use generated::settings as profile;
pub use generated::settings::*;
pub use generated::types::*;
pub use generated::{NM_VERSION, proxies, types};

/// zbus, re-exported: its [`Connection`](zbus::Connection) is the entry point to every proxy,
/// and its error type is what they return.
pub use zbus;
