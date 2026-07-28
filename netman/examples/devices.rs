//! List every network device NetworkManager knows about, and what it is doing.
//!
//! Run with `cargo run --example devices`.

use netman::{
    DeviceProxy, DeviceState, NetworkManagerProxy, ObjectPathSliceExt, ProxyExt, WirelessProxy,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::system().await?;
    let nm = NetworkManagerProxy::new(&connection).await?;

    println!(
        "NetworkManager {} — state {:?}",
        nm.version().await?,
        nm.state().await?
    );
    println!(
        "connectivity: {:?}, metered: {:?}",
        nm.connectivity().await?,
        nm.metered().await?
    );
    println!("capabilities: {:?}", nm.capabilities().await?);

    let devices: Vec<DeviceProxy> = nm.get_devices().await?.proxies(&connection).await?;
    for device in devices {
        let state = device.state().await?;
        println!(
            "\n{} ({:?}) — {:?}",
            device.interface().await?,
            device.device_type().await?,
            state,
        );
        println!("  capabilities: {:?}", device.capabilities().await?);
        println!(
            "  managed: {}, metered: {:?}",
            device.managed().await?,
            device.metered().await?
        );

        // The same object also carries the wireless interface when it is a Wi-Fi card.
        let wireless: WirelessProxy = device.cast().await?;
        if let Ok(access_points) = wireless.get_access_points().await {
            println!("  {} access point(s) seen", access_points.len());
        }

        if state == DeviceState::Activated {
            println!("  active connection: {}", device.active_connection().await?);
        }
    }

    Ok(())
}
