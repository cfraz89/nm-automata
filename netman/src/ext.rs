//! Navigation between NetworkManager objects.
//!
//! Almost everything NetworkManager returns is an object path: `Devices` is a list of them,
//! `ActiveConnection` is one, and a single device object carries `Device`, `Device.Wired` and
//! `Device.Statistics` at once. These two traits turn those paths and cross-interface hops
//! into typed proxies, and are generic over every generated proxy, so nothing here is written
//! per interface.

use std::future::Future;

use zbus::proxy::{Builder, Defaults};
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zbus::{Connection, Proxy, Result};

/// Build proxies for the objects an object path points at.
pub trait ObjectPathExt {
    /// The proxy for one of this object's interfaces.
    ///
    /// ```no_run
    /// # use netman::{DeviceProxy, NetworkManagerProxy, ObjectPathExt};
    /// # async fn example(nm: NetworkManagerProxy<'_>) -> zbus::Result<()> {
    /// let connection = zbus::Connection::system().await?;
    /// for path in nm.get_devices().await? {
    ///     let device: DeviceProxy = path.proxy(&connection).await?;
    ///     println!("{}", device.interface().await?);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn proxy<P>(&self, connection: &Connection) -> impl Future<Output = Result<P>>
    where
        P: Defaults + From<Proxy<'static>>;
}

impl ObjectPathExt for ObjectPath<'_> {
    async fn proxy<P>(&self, connection: &Connection) -> Result<P>
    where
        P: Defaults + From<Proxy<'static>>,
    {
        Builder::<'static, P>::new(connection)
            .path(ObjectPath::to_owned(self))?
            .build()
            .await
    }
}

/// Build proxies for a whole list of object paths, as most NetworkManager properties return.
pub trait ObjectPathSliceExt {
    /// The proxies for one interface of every object in the list.
    ///
    /// ```no_run
    /// # use netman::{DeviceProxy, NetworkManagerProxy, ObjectPathSliceExt};
    /// # async fn example(nm: NetworkManagerProxy<'_>) -> zbus::Result<()> {
    /// let connection = zbus::Connection::system().await?;
    /// let devices: Vec<DeviceProxy> = nm.get_devices().await?.proxies(&connection).await?;
    /// # Ok(())
    /// # }
    /// ```
    fn proxies<P>(&self, connection: &Connection) -> impl Future<Output = Result<Vec<P>>>
    where
        P: Defaults + From<Proxy<'static>>;
}

impl ObjectPathSliceExt for [OwnedObjectPath] {
    async fn proxies<P>(&self, connection: &Connection) -> Result<Vec<P>>
    where
        P: Defaults + From<Proxy<'static>>,
    {
        let mut out = Vec::with_capacity(self.len());
        for path in self {
            out.push(path.proxy(connection).await?);
        }
        Ok(out)
    }
}

impl ObjectPathSliceExt for Vec<OwnedObjectPath> {
    fn proxies<P>(&self, connection: &Connection) -> impl Future<Output = Result<Vec<P>>>
    where
        P: Defaults + From<Proxy<'static>>,
    {
        self.as_slice().proxies(connection)
    }
}

/// Move between the interfaces a single NetworkManager object implements.
pub trait ProxyExt<'p> {
    /// Another interface of the same object, on the same connection.
    ///
    /// ```no_run
    /// # use netman::{DeviceProxy, ProxyExt, WirelessProxy};
    /// # async fn example(device: DeviceProxy<'_>) -> zbus::Result<()> {
    /// let wireless: WirelessProxy = device.cast().await?;
    /// for ap in wireless.get_all_access_points().await? {
    ///     println!("{ap}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Whether the object really implements the target interface is only discovered when a
    /// call is made on the returned proxy: a wired device cast to [`WirelessProxy`] builds
    /// fine and fails on first use.
    ///
    /// [`WirelessProxy`]: crate::WirelessProxy
    fn cast<P>(&self) -> impl Future<Output = Result<P>>
    where
        P: Defaults + From<Proxy<'static>>;
}

impl<'p, T> ProxyExt<'p> for T
where
    T: AsRef<Proxy<'p>>,
{
    async fn cast<P>(&self) -> Result<P>
    where
        P: Defaults + From<Proxy<'static>>,
    {
        let proxy = self.as_ref();
        proxy.path().proxy(proxy.connection()).await
    }
}
