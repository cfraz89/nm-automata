//! A property with a stronger type than the wire gives it still has to round-trip.
//!
//! `802-11-wireless.mode` is a `string` and `ssid` a `byte array`, and this crate reads them as
//! an enumeration and as an [`Ssid`]. Neither is allowed to cost anything: a profile read from
//! NetworkManager and written straight back must carry exactly what it arrived with, including
//! values this binding has never heard of, since dropping or normalising one would rewrite the
//! user's profile behind their back.

use std::collections::HashMap;

use nm_automata::settings::{ProfileMap, SettingMap};
use nm_automata::{
    ActivationBinding, ActivationOptions, ActivationPersist, AssignedMacAddress, ConnectionProfile,
    Ipv4Method, Ipv6Method, MacAddress, Ssid, WirelessMode, WirelessSecurityProto, WirelessSetting,
};
use zbus::zvariant::{OwnedValue, Value};

fn value(value: impl Into<Value<'static>>) -> OwnedValue {
    OwnedValue::try_from(value.into()).expect("a value with no file descriptors")
}

/// A profile as NetworkManager would send it, with the given Wi-Fi properties.
fn wireless(properties: impl IntoIterator<Item = (&'static str, OwnedValue)>) -> ProfileMap {
    let setting: SettingMap = properties
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    HashMap::from([("802-11-wireless".to_owned(), setting)])
}

#[test]
fn reads_the_values_it_knows() {
    let profile = ConnectionProfile::from_map(&wireless([
        ("mode", value("infrastructure")),
        ("ssid", value(b"home wifi".to_vec())),
        ("bssid", value(vec![0x4c_u8, 0x79, 0x6e, 0xda, 0x29, 0x47])),
    ]))
    .expect("the profile decodes");

    let wireless = profile.wireless.expect("the setting is there");
    assert_eq!(wireless.mode, Some(WirelessMode::Infrastructure));
    assert_eq!(wireless.ssid, Some(Ssid::from("home wifi")));
    assert_eq!(
        wireless.bssid.map(|address| address.to_string()),
        Some("4C:79:6E:DA:29:47".to_owned())
    );
}

#[test]
fn keeps_a_value_it_does_not_know() {
    // Whatever a newer NetworkManager grows, reading it must not fail and writing it back must
    // not change it.
    let profile = ConnectionProfile::from_map(&wireless([("mode", value("quantum-entangled"))]))
        .expect("an unknown value still decodes");

    let mode = profile.wireless.as_ref().and_then(|w| w.mode.clone());
    assert_eq!(mode, Some(WirelessMode::Other("quantum-entangled".into())));
    assert!(mode.expect("the mode is set").is_unknown());

    let written = profile.to_map().expect("the profile encodes");
    assert_eq!(
        written["802-11-wireless"]["mode"],
        value("quantum-entangled")
    );
}

#[test]
fn keeps_an_ssid_that_is_not_text() {
    // An SSID is octets, not a string: one in some other encoding has to survive being read.
    let raw = b"caf\xe9 wifi".to_vec();
    let profile = ConnectionProfile::from_map(&wireless([("ssid", value(raw.clone()))]))
        .expect("the profile decodes");

    let ssid = profile.wireless.as_ref().and_then(|w| w.ssid.clone());
    assert_eq!(ssid.as_ref().map(Ssid::to_str), Some(None));
    assert_eq!(ssid.map(Ssid::into_bytes), Some(raw.clone()));

    let written = profile.to_map().expect("the profile encodes");
    assert_eq!(written["802-11-wireless"]["ssid"], value(raw));
}

#[test]
fn keeps_the_elements_of_a_list_it_only_half_knows() {
    let profile = ConnectionProfile::from_map(&HashMap::from([(
        "802-11-wireless-security".to_owned(),
        SettingMap::from([(
            "proto".to_owned(),
            value(vec!["rsn".to_owned(), "wpa4".to_owned()]),
        )]),
    )]))
    .expect("the profile decodes");

    let proto = profile
        .wireless_security
        .as_ref()
        .and_then(|s| s.proto.clone());
    assert_eq!(
        proto,
        Some(vec![
            WirelessSecurityProto::Rsn,
            WirelessSecurityProto::Other("wpa4".into()),
        ])
    );

    let written = profile.to_map().expect("the profile encodes");
    assert_eq!(
        written["802-11-wireless-security"]["proto"],
        value(vec!["rsn".to_owned(), "wpa4".to_owned()])
    );
}

/// The two address families enumerate their `method` separately, because they differ.
#[test]
fn each_address_family_has_its_own_methods() {
    let profile = |setting: &str, method: &'static str| {
        ConnectionProfile::from_map(&HashMap::from([(
            setting.to_owned(),
            SettingMap::from([("method".to_owned(), value(method))]),
        )]))
        .expect("the profile decodes")
    };

    assert_eq!(
        profile("ipv4", "link-local").ipv4.and_then(|ip| ip.method),
        Some(Ipv4Method::LinkLocal)
    );
    // `ignore` and `dhcp` are IPv6 only; NetworkManager rejects either on an IPv4 setting, and
    // the two being separate types is what says so.
    assert_eq!(
        profile("ipv6", "ignore").ipv6.and_then(|ip| ip.method),
        Some(Ipv6Method::Ignore)
    );
    assert_eq!(
        profile("ipv4", "ignore").ipv4.and_then(|ip| ip.method),
        Some(Ipv4Method::Other("ignore".to_owned()))
    );
}

/// `assigned-mac-address` is one string holding either an address or a way of choosing one.
#[test]
fn reads_an_address_and_a_way_of_choosing_one_from_the_same_property() {
    let profile = ConnectionProfile::from_map(&wireless([(
        "assigned-mac-address",
        value("4C:79:6E:DA:29:47"),
    )]))
    .expect("the profile decodes");
    let assigned = profile
        .wireless
        .as_ref()
        .and_then(|w| w.assigned_mac_address.clone());
    assert_eq!(
        assigned.as_ref().and_then(AssignedMacAddress::address),
        Some(&MacAddress::from([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47]))
    );

    // And back byte for byte, since NetworkManager sends the canonical form.
    let written = profile.to_map().expect("the profile encodes");
    assert_eq!(
        written["802-11-wireless"]["assigned-mac-address"],
        value("4C:79:6E:DA:29:47")
    );

    let stable =
        ConnectionProfile::from_map(&wireless([("assigned-mac-address", value("stable-ssid"))]))
            .expect("the profile decodes");
    assert_eq!(
        stable
            .wireless
            .as_ref()
            .and_then(|w| w.assigned_mac_address.clone()),
        Some(AssignedMacAddress::StableSsid)
    );
    assert_eq!(
        stable.to_map().expect("the profile encodes")["802-11-wireless"]["assigned-mac-address"],
        value("stable-ssid")
    );
}

/// What a caller writes has to arrive as the string and the octets NetworkManager expects.
#[test]
fn writes_what_networkmanager_expects() {
    let mut profile = ConnectionProfile::new("home wifi");
    profile.wireless = Some(WirelessSetting {
        ssid: Some(Ssid::from("home wifi")),
        mode: Some(WirelessMode::Ap),
        bssid: Some(MacAddress::from([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47])),
        ..Default::default()
    });

    let written = profile.to_map().expect("the profile encodes");
    let wireless = &written["802-11-wireless"];
    assert_eq!(wireless["mode"], value("ap"));
    assert_eq!(wireless["ssid"], value(b"home wifi".to_vec()));
    assert_eq!(
        wireless["bssid"],
        value(vec![0x4c_u8, 0x79, 0x6e, 0xda, 0x29, 0x47])
    );
}

/// The options of `AddAndActivateConnection2` are a dictionary with a shape, not a bag of
/// variants — and one this crate writes rather than reads, so what goes out has to be right.
#[test]
fn activation_options_travel_as_the_dictionary_networkmanager_reads() {
    let options = ActivationOptions::default()
        .persist(ActivationPersist::Volatile)
        .bind_activation(ActivationBinding::DbusClient);

    let map = options.to_map().expect("the options encode");
    assert_eq!(map["persist"], value("volatile"));
    assert_eq!(map["bind-activation"], value("dbus-client"));

    assert_eq!(ActivationOptions::from_map(&map).unwrap(), options);
    // And the signature the method's argument expects.
    assert_eq!(
        <ActivationOptions as zbus::zvariant::Type>::SIGNATURE.to_string(),
        "a{sv}"
    );
}
