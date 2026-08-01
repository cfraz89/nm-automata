//! A profile must go on the wire the same way every time it is serialised.
//!
//! zvariant serialises a value twice - once to size the buffer, then again to fill it - so a
//! `Serialize` impl whose byte layout varies between calls writes a length that does not match
//! the bytes after it. A strict bus does not tolerate that: `dbus-broker` closes the connection
//! with "sent a message with an invalid body", which takes down every later call on it too.
//! The maps behind a profile are `HashMap`s built afresh by `to_map`, and their iteration order
//! is not stable between instances, so this is the property that keeps them in line.

use nm_automata::{
    AddressData, ConnectionProfile, Ipv4Method, Ipv4Setting, WirelessBand, WirelessMode,
    WirelessSetting,
};
use zbus::zvariant::{LE, serialized::Context, serialized_size, to_bytes};

/// Enough settings and properties that a varying order would show up as a varying length.
fn profile() -> ConnectionProfile {
    ConnectionProfile::new("nm-automata")
        .wireless(
            WirelessSetting::default()
                .ssid("nm-automata")
                .mode(WirelessMode::Infrastructure)
                .band(WirelessBand::A)
                .hidden(true),
        )
        .ipv4(Ipv4Setting::default().method(Ipv4Method::Auto))
}

#[test]
fn a_profile_serialises_to_the_same_bytes_every_time() {
    let ctxt = Context::new_dbus(LE, 0);
    let profile = profile();
    let first = to_bytes(ctxt, &profile).expect("serialise");
    for _ in 0..200 {
        let again = to_bytes(ctxt, &profile).expect("serialise");
        assert_eq!(first.bytes(), again.bytes(), "profile serialisation varies");
    }
}

/// The size reserved for a value has to be the size actually written into it.
#[test]
fn a_profile_fills_exactly_the_space_it_asks_for() {
    let ctxt = Context::new_dbus(LE, 0);
    for _ in 0..200 {
        let profile = profile();
        let reserved = serialized_size(ctxt, &profile).expect("size");
        let written = to_bytes(ctxt, &profile).expect("serialise");
        assert_eq!(
            reserved.size(),
            written.len(),
            "reserved {} bytes, wrote {}",
            reserved.size(),
            written.len()
        );
    }
}

/// The same holds for the dictionary-valued properties, which are built the same way.
#[test]
fn a_vardict_fills_exactly_the_space_it_asks_for() {
    let ctxt = Context::new_dbus(LE, 0);
    for _ in 0..200 {
        let address = AddressData {
            address: Some("192.168.1.2".into()),
            prefix: Some(24),
            ..Default::default()
        };
        let reserved = serialized_size(ctxt, &address).expect("size");
        let written = to_bytes(ctxt, &address).expect("serialise");
        assert_eq!(reserved.size(), written.len());
    }
}
