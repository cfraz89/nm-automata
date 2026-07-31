//! Watch NetworkManager for things appearing and going away.
//!
//! The signal streams yield the objects themselves rather than paths, so a new access point can
//! be asked what it is without resolving anything first.
//!
//! Run with `cargo run --example watch`. Access points come and go on their own as the Wi-Fi
//! scan list is refreshed; devices need one adding, e.g. `sudo ip link add dummy0 type dummy`.

use futures_util::StreamExt;
use nm_automata::{AnyDevice, NetworkManagerProxy};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::system().await?;
    let nm = NetworkManagerProxy::new(&connection).await?;

    let mut wireless = None;
    for device in nm.get_devices().await? {
        if let AnyDevice::Wireless(wifi) = device.specialise().await? {
            wireless = Some(wifi);
            break;
        }
    }

    let mut devices_added = nm.receive_device_added().await?;
    let mut devices_removed = nm.receive_device_removed_path().await?;

    println!("watching (ctrl-c to stop)");
    match wireless {
        Some(wifi) => {
            let mut seen = wifi.receive_access_point_added().await?;
            let mut lost = wifi.receive_access_point_removed().await?;
            loop {
                tokio::select! {
                    Some(access_point) = seen.next() => {
                        let access_point = access_point?;
                        // A proxy, not a path: usable straight away.
                        println!(
                            "+ ap {:?} {} dBm",
                            access_point.ssid().await?,
                            access_point.strength().await? as i8,
                        );
                    }
                    Some(access_point) = lost.next() => {
                        // Already gone, so only its identity is left to report.
                        println!("- ap {}", access_point?.object_path());
                    }
                    Some(device) = devices_added.next() => {
                        println!("+ device {}", device?.interface().await?);
                    }
                    Some(signal) = devices_removed.next() => {
                        println!("- device {}", signal.args()?.device_path());
                    }
                    else => break,
                }
            }
        }
        None => println!("no wireless device to watch"),
    }
    Ok(())
}
