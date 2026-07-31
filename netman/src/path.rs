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
    /// ```no_run
    /// # use netman::{NetworkManagerProxy, NmPathSliceExt};
    /// # async fn example(nm: NetworkManagerProxy<'_>) -> zbus::Result<()> {
    /// let connection = zbus::Connection::system().await?;
    /// for device in nm.get_devices().await?.connect_all(&connection).await? {
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
