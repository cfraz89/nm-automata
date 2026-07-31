//! Read every saved connection profile, and check it survives a round trip.
//!
//! Reading a profile, converting it to typed structs and converting it back must reproduce the
//! map NetworkManager sent, or a read-modify-write would corrupt the profile. This walks the
//! real profiles on this machine and reports any property that does not survive.
//!
//! Run with `cargo run --example profiles`. It only reads.

use nm_automata::settings::ProfileMap;
use nm_automata::{ConnectionProfile, SettingsConnectionProxy, SettingsProxy};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = zbus::Connection::system().await?;
    let settings = SettingsProxy::new(&connection).await?;

    let profiles: Vec<SettingsConnectionProxy> = settings.list_connections().await?;
    println!("{} saved profile(s)\n", profiles.len());

    let mut mismatches = 0;
    for profile in profiles {
        // The raw wire form, fetched through the underlying zbus proxy, against the typed
        // form the generated method returns. Comparing the two checks the whole path.
        let sent: ProfileMap = profile.inner().call("GetSettings", &()).await?;
        let typed: ConnectionProfile = profile.get_settings().await?;
        let returned = typed.to_map()?;

        let id = typed
            .connection
            .as_ref()
            .and_then(|c| c.id.clone())
            .unwrap_or_else(|| "<unnamed>".into());
        let kind = typed
            .connection
            .as_ref()
            .and_then(|c| c.type_.clone())
            .unwrap_or_default();
        println!("{id} ({kind})");

        for (setting, properties) in &sent {
            let Some(back) = returned.get(setting) else {
                println!("  LOST SETTING {setting}");
                mismatches += 1;
                continue;
            };
            for (property, value) in properties {
                match back.get(property) {
                    None => {
                        println!("  LOST {setting}.{property}");
                        mismatches += 1;
                    }
                    Some(returned) if returned != value => {
                        println!("  CHANGED {setting}.{property}: {value:?} -> {returned:?}");
                        mismatches += 1;
                    }
                    Some(_) => {}
                }
            }
        }
        if let Some(ipv4) = &typed.ipv4 {
            println!("  ipv4.method={:?} dns={:?}", ipv4.method, ipv4.dns);
        }
        if let Some(wireless) = &typed.wireless {
            println!(
                "  wireless.ssid={:?} mode={:?} band={:?}",
                wireless.ssid, wireless.mode, wireless.band
            );
        }
    }

    println!(
        "\n{}",
        if mismatches == 0 {
            "every property round-tripped".to_owned()
        } else {
            format!("{mismatches} propert(ies) did not round-trip")
        }
    );
    Ok(())
}
