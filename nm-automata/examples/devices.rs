//! List every network device NetworkManager knows about, and what it is doing.
//!
//! Run with `cargo run --example devices`.

use nm_automata::{AnyDevice, DeviceState, NetworkManagerProxy};

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

    // `get_devices` hands back the devices themselves, built on the connection `nm` holds.
    for device in nm.get_devices().await? {
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

        // Narrowed once from the reported type, so each branch has a proxy that is certain
        // to work rather than one that might fail on first use.
        match device.specialise().await? {
            AnyDevice::Wireless(wifi) => {
                println!(
                    "  {} access point(s) seen",
                    wifi.get_access_points().await?.len()
                );
                println!(
                    "  mode {:?}, bitrate {}",
                    wifi.mode().await?,
                    wifi.bitrate().await?
                );
            }
            AnyDevice::Wired(wired) => {
                println!(
                    "  speed {} Mb/s, carrier {}",
                    wired.speed().await?,
                    wired.carrier().await?
                );
            }
            AnyDevice::Bluetooth(bt) => println!("  bt name {:?}", bt.name().await?),
            other => println!("  no specialised interface for {:?}", other.device_type()),
        }

        if state == DeviceState::Activated {
            let active = device.active_connection().await?;
            println!("  active connection: {:?}", active.state().await?);

            // Typed dictionaries: addresses and routes come back with real fields rather
            // than as maps of variants.
            let ip4 = device.ip4_config().await?;
            for address in ip4.address_data().await? {
                println!(
                    "  address {}/{}",
                    address.address.unwrap_or_default(),
                    address.prefix.unwrap_or_default()
                );
            }
            for route in ip4.route_data().await?.iter().take(2) {
                println!(
                    "  route {}/{} via {} metric {}",
                    route.dest.clone().unwrap_or_default(),
                    route.prefix.unwrap_or_default(),
                    route.next_hop.clone().unwrap_or_else(|| "-".into()),
                    route.metric.unwrap_or_default()
                );
            }
        }
    }

    Ok(())
}
