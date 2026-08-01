//! Object paths that know what they point at.
//!
//! Almost everything NetworkManager returns is an object path, and a bare
//! [`OwnedObjectPath`](zbus::zvariant::OwnedObjectPath) says nothing about which kind of object
//! is at the other end. The generated path types do: a `DevicePath` is a device, and it will
//! build you a `DeviceProxy` and nothing else.
//!
//! They cost nothing on the wire — each is a newtype over the path, with the same signature —
//! and they make the mistake of handing a device path to something expecting an access point a
//! compile error rather than a runtime one.
//!
//! # Getting one back out
//!
//! Most members hand you the object rather than its path, so a path usually appears when you
//! need to give one to a member that takes one. Every proxy knows its own:
//!
//! ```no_run
//! # use nm_automata::{NetworkManagerProxy, SettingsProxy};
//! # async fn example(connection: &zbus::Connection) -> zbus::Result<()> {
//! let nm = NetworkManagerProxy::new(connection).await?;
//! let settings = SettingsProxy::new(connection).await?;
//!
//! let device = nm.get_devices().await?.remove(0);
//! let profile = settings.list_connections().await?.remove(0);
//!
//! let active = nm
//!     .activate_connection(
//!         &profile.object_path(),
//!         &device.object_path(),
//!         &Default::default(),
//!     )
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! It is `object_path` rather than `path` because `Device.Path` is a real property — the one
//! udev calls `ID_PATH`.
//!
//! The `_path` and `_paths` members are the raw form of anything that returns objects:
//! `get_device_paths` beside `get_devices`. They are rarely what you want, but they are not
//! merely redundant — a property's change stream is named after them, so watching
//! `ActiveConnection` means `receive_active_connection_path_changed`.

/// The path NetworkManager sends where there is no object.
///
/// D-Bus has no null, so a property that points at nothing still has to point somewhere. Every
/// NetworkManager object lives under `/org/freedesktop/NetworkManager`, which leaves the root
/// path free to mean "none", and that is what it means: a device with no address configuration
/// reports its `Ip4Config` this way, and so does a Wi-Fi device that is not associated.
///
/// # Why this one is decided for you
///
/// [`NonEmpty`](crate::NonEmpty) leaves the empty string to the caller, because nothing
/// upstream says which properties mean absent by it and plenty of them mean something else. A
/// path is not like that. No object of any interface is at `/`, so a proxy built there could
/// only fail on its first call — there is no second meaning to lose. So the accessors that turn
/// a property's path into an object hand back [`None`] instead:
///
/// ```no_run
/// # use nm_automata::WirelessProxy;
/// # async fn example(wifi: WirelessProxy<'_>) -> zbus::Result<()> {
/// match wifi.active_access_point().await? {
///     Some(ap) => println!("associated with {}", ap.ssid().await?),
///     None => println!("not associated"),
/// }
/// # Ok(())
/// # }
/// ```
///
/// Only those. A method reports failure as an error rather than by handing back `/`, a list
/// leaves an object out rather than listing `/`, and a member that *takes* a path is untouched:
/// NetworkManager documents `/` as an argument meaning "no specific object", which is a value
/// to pass rather than one to read.
pub const NULL_PATH: &str = "/";

/// A path that knows the proxy it builds.
pub trait NmPath {
    /// The proxy for an object at this path.
    type Proxy;

    /// The D-Bus interface an object at this path implements.
    const INTERFACE: &'static str;

    /// Build the proxy for the object at this path.
    fn connect(
        &self,
        connection: &zbus::Connection,
    ) -> impl std::future::Future<Output = zbus::Result<Self::Proxy>>;
}

/// Build proxies for a whole list of paths, as most NetworkManager members return.
pub trait NmPathSliceExt {
    type Proxy;

    /// Build the proxy for every object in the list.
    ///
    /// Most members give you the objects directly — `get_devices` already returns proxies —
    /// so this is for when you have held on to the paths themselves.
    ///
    /// ```no_run
    /// # use nm_automata::{NetworkManagerProxy, NmPathSliceExt};
    /// # async fn example(nm: NetworkManagerProxy<'_>) -> zbus::Result<()> {
    /// let connection = zbus::Connection::system().await?;
    /// for device in nm.get_device_paths().await?.connect_all(&connection).await? {
    ///     println!("{}", device.interface().await?);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn connect_all(
        &self,
        connection: &zbus::Connection,
    ) -> impl std::future::Future<Output = zbus::Result<Vec<Self::Proxy>>>;
}

impl<P: NmPath> NmPathSliceExt for [P] {
    type Proxy = P::Proxy;

    async fn connect_all(&self, connection: &zbus::Connection) -> zbus::Result<Vec<P::Proxy>> {
        let mut out = Vec::with_capacity(self.len());
        for path in self {
            out.push(path.connect(connection).await?);
        }
        Ok(out)
    }
}

impl<P: NmPath> NmPathSliceExt for Vec<P> {
    type Proxy = P::Proxy;

    fn connect_all(
        &self,
        connection: &zbus::Connection,
    ) -> impl std::future::Future<Output = zbus::Result<Vec<P::Proxy>>> {
        self.as_slice().connect_all(connection)
    }
}

/// Define a path type for one interface.
macro_rules! nm_path {
    (
        $(#$meta:tt)*
        pub struct $name:ident: $interface:literal;
    ) => {
        $(#$meta)*
        ///
        /// A [`zbus::zvariant::OwnedObjectPath`] that is known to point at an object of this
        /// kind. `connect` builds the proxy for it.
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(zbus::zvariant::OwnedObjectPath);

        impl $name {
            /// The D-Bus interface an object at this path implements.
            pub const INTERFACE: &'static str = $interface;

            /// The path itself.
            pub fn as_object_path(&self) -> &zbus::zvariant::ObjectPath<'static> {
                &self.0
            }

            /// The path itself, given up.
            pub fn into_object_path(self) -> zbus::zvariant::OwnedObjectPath {
                self.0
            }

            /// Whether this is [`/`](crate::NULL_PATH), which is how NetworkManager says
            /// there is no object.
            pub fn is_null(&self) -> bool {
                self.0.as_str() == $crate::path::NULL_PATH
            }

            /// The path, or [`None`] if it is [`/`](Self::is_null).
            pub fn non_null(self) -> Option<Self> {
                match self.is_null() {
                    true => None,
                    false => Some(self),
                }
            }
        }

        impl std::ops::Deref for $name {
            type Target = zbus::zvariant::ObjectPath<'static>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(self.0.as_str(), f)
            }
        }

        impl From<zbus::zvariant::OwnedObjectPath> for $name {
            fn from(path: zbus::zvariant::OwnedObjectPath) -> Self {
                Self(path)
            }
        }

        impl From<$name> for zbus::zvariant::OwnedObjectPath {
            fn from(path: $name) -> Self {
                path.0
            }
        }

        impl zbus::zvariant::Type for $name {
            const SIGNATURE: &'static zbus::zvariant::Signature =
                <zbus::zvariant::OwnedObjectPath as zbus::zvariant::Type>::SIGNATURE;
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serde::Serialize::serialize(&self.0, serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <zbus::zvariant::OwnedObjectPath as serde::Deserialize>::deserialize(deserializer)
                    .map(Self)
            }
        }

        impl TryFrom<zbus::zvariant::OwnedValue> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {
                zbus::zvariant::OwnedObjectPath::try_from(value).map(Self)
            }
        }

        // `Vec<T>` decodes element by element through `Value`, so a list of paths needs this
        // as well as the `OwnedValue` conversion above.
        impl<'a> TryFrom<zbus::zvariant::Value<'a>> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::Value<'a>) -> Result<Self, Self::Error> {
                zbus::zvariant::ObjectPath::try_from(value).map(|path| Self(path.into()))
            }
        }

        impl From<$name> for zbus::zvariant::Value<'_> {
            fn from(path: $name) -> Self {
                zbus::zvariant::Value::from(path.0)
            }
        }
    };
}

pub(crate) use nm_path;

#[cfg(test)]
mod tests {
    use crate::{AccessPointPath, NULL_PATH};
    use zbus::zvariant::{ObjectPath, OwnedObjectPath};

    fn path(text: &str) -> AccessPointPath {
        AccessPointPath::from(OwnedObjectPath::from(
            ObjectPath::try_from(text).expect("a valid object path"),
        ))
    }

    #[test]
    fn the_root_path_is_the_absent_one() {
        assert!(path(NULL_PATH).is_null());
        assert_eq!(path(NULL_PATH).non_null(), None);

        let real = path("/org/freedesktop/NetworkManager/AccessPoint/1");
        assert!(!real.is_null());
        assert_eq!(real.clone().non_null(), Some(real));
    }
}
