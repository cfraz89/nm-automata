//! Hardware addresses, in the notation everything but the wire uses.

use std::fmt;
use std::str::FromStr;

use crate::repr::nm_bytes_repr;

/// A hardware address, as a connection profile carries it.
///
/// NetworkManager sends these as raw bytes over D-Bus while writing them as hex digits and
/// colons everywhere a person sees them — its own documentation, `nmcli`, the keyfiles on disk.
/// This is the same bytes with that notation attached:
///
/// ```
/// # use nm_automata::MacAddress;
/// let address: MacAddress = "4c:79:6e:da:29:47".parse()?;
/// assert_eq!(address.to_string(), "4C:79:6E:DA:29:47");
/// assert_eq!(address.as_bytes(), &[0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47]);
/// # Ok::<(), nm_automata::InvalidMacAddress>(())
/// ```
///
/// The length is not fixed at six. NetworkManager uses the same field for the twenty-byte
/// hardware address of an IPoIB device, so an address is however many octets the device it
/// belongs to has.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MacAddress(Vec<u8>);

impl MacAddress {
    /// The address these bytes are.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The octets, as NetworkManager sends them.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Take the octets back out.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// How many octets the address is — six for Ethernet and Wi-Fi, twenty for IPoIB.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there is no address at all, which for a profile means "any device".
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// In upper case with colons, which is how NetworkManager writes one.
impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MacAddress({self})")
    }
}

/// An address that is not hex digits in groups of two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidMacAddress {
    text: String,
}

impl fmt::Display for InvalidMacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a hardware address: expected hex digits in pairs, separated by `:` or \
             `-`, as in `4C:79:6E:DA:29:47`",
            self.text
        )
    }
}

impl std::error::Error for InvalidMacAddress {}

impl FromStr for MacAddress {
    type Err = InvalidMacAddress;

    /// Reads `4C:79:6E:DA:29:47`, and the same with `-` between the octets.
    ///
    /// The length is not checked against any particular kind of device: NetworkManager is the
    /// one that knows how long the address of the device a profile names should be, and it
    /// rejects a profile whose address is the wrong length for it.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidMacAddress {
            text: text.to_owned(),
        };
        if text.is_empty() {
            return Err(invalid());
        }
        text.split([':', '-'])
            .map(|octet| {
                if octet.len() != 2 {
                    return Err(invalid());
                }
                u8::from_str_radix(octet, 16).map_err(|_| invalid())
            })
            .collect::<Result<Vec<u8>, _>>()
            .map(Self)
    }
}

impl TryFrom<&str> for MacAddress {
    type Error = InvalidMacAddress;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl<const N: usize> From<[u8; N]> for MacAddress {
    fn from(bytes: [u8; N]) -> Self {
        Self(bytes.to_vec())
    }
}

nm_bytes_repr!(MacAddress);

/// What a device's MAC address should be on activation — `assigned-mac-address`.
///
/// The property travels as a string that is either an address or one of a handful of words
/// asking NetworkManager to work one out, so it is neither a [`MacAddress`] nor an enumeration
/// of fixed values but both at once:
///
/// ```
/// # use nm_automata::{AssignedMacAddress, MacAddress};
/// assert_eq!(AssignedMacAddress::from("random"), AssignedMacAddress::Random);
/// assert_eq!(
///     AssignedMacAddress::from("4C:79:6E:DA:29:47").address(),
///     Some(&MacAddress::from([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47])),
/// );
/// ```
///
/// Note that libnm and `nmcli` call this property `cloned-mac-address`, which on D-Bus is the
/// name of the deprecated field that can only hold an explicit address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssignedMacAddress {
    /// Leave the address alone.
    Preserve,
    /// Use the permanent hardware address of the device.
    Permanent,
    /// A new random address on every connect.
    Random,
    /// An address hashed from `connection.stable-id` and a machine-dependent key.
    Stable,
    /// An address hashed from the SSID, as a stable-id of `${NETWORK_SSID}` would give.
    ///
    /// Wi-Fi only: NetworkManager rejects it on a wired profile.
    StableSsid,
    /// Use exactly this address.
    Address(MacAddress),
    /// A value this binding does not know about, kept as it came.
    Other(String),
}

impl AssignedMacAddress {
    /// The address this asks for, if it names one rather than a way of choosing one.
    pub fn address(&self) -> Option<&MacAddress> {
        match self {
            Self::Address(address) => Some(address),
            _ => None,
        }
    }

    /// Whether this is a value the binding does not recognise.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Other(_))
    }

    /// Decode the string NetworkManager sent.
    ///
    /// Anything that is neither a special value nor an address it can parse is kept verbatim,
    /// so writing the profile back sends exactly what was read.
    pub fn from_wire(text: impl Into<String>) -> Self {
        let text = text.into();
        match text.as_str() {
            "preserve" => Self::Preserve,
            "permanent" => Self::Permanent,
            "random" => Self::Random,
            "stable" => Self::Stable,
            "stable-ssid" => Self::StableSsid,
            address => match address.parse() {
                Ok(address) => Self::Address(address),
                Err(_) => Self::Other(text),
            },
        }
    }
}

/// As NetworkManager writes it: one of the special values, or the address in canonical form.
///
/// An address read from NetworkManager comes back byte for byte, since the canonical form —
/// upper case, colons, no elided zeroes — is the one it sends.
impl fmt::Display for AssignedMacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preserve => f.write_str("preserve"),
            Self::Permanent => f.write_str("permanent"),
            Self::Random => f.write_str("random"),
            Self::Stable => f.write_str("stable"),
            Self::StableSsid => f.write_str("stable-ssid"),
            Self::Address(address) => write!(f, "{address}"),
            Self::Other(text) => f.write_str(text),
        }
    }
}

/// Infallible: a string this binding does not recognise is a value in its own right.
impl FromStr for AssignedMacAddress {
    type Err = std::convert::Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_wire(text))
    }
}

impl From<&str> for AssignedMacAddress {
    fn from(text: &str) -> Self {
        Self::from_wire(text)
    }
}

impl From<String> for AssignedMacAddress {
    fn from(text: String) -> Self {
        Self::from_wire(text)
    }
}

impl From<MacAddress> for AssignedMacAddress {
    fn from(address: MacAddress) -> Self {
        Self::Address(address)
    }
}

impl From<AssignedMacAddress> for String {
    fn from(value: AssignedMacAddress) -> Self {
        match value {
            AssignedMacAddress::Other(text) => text,
            value => value.to_string(),
        }
    }
}

impl zbus::zvariant::Type for AssignedMacAddress {
    const SIGNATURE: &'static zbus::zvariant::Signature =
        <String as zbus::zvariant::Type>::SIGNATURE;
}

impl serde::Serialize for AssignedMacAddress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for AssignedMacAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <String as serde::Deserialize>::deserialize(deserializer).map(Self::from_wire)
    }
}

impl From<AssignedMacAddress> for zbus::zvariant::Value<'_> {
    fn from(value: AssignedMacAddress) -> Self {
        zbus::zvariant::Value::from(String::from(value))
    }
}

impl TryFrom<zbus::zvariant::OwnedValue> for AssignedMacAddress {
    type Error = zbus::zvariant::Error;

    fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {
        String::try_from(value).map(Self::from_wire)
    }
}

impl TryFrom<zbus::zvariant::Value<'_>> for AssignedMacAddress {
    type Error = zbus::zvariant::Error;

    fn try_from(value: zbus::zvariant::Value<'_>) -> Result<Self, Self::Error> {
        String::try_from(value).map(Self::from_wire)
    }
}

#[cfg(test)]
mod tests {
    use super::{AssignedMacAddress, MacAddress};

    #[test]
    fn reads_both_separators_in_either_case() {
        let expected = MacAddress::from([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47]);
        assert_eq!("4C:79:6E:DA:29:47".parse(), Ok(expected.clone()));
        assert_eq!("4c:79:6e:da:29:47".parse(), Ok(expected.clone()));
        assert_eq!("4c-79-6e-da-29-47".parse(), Ok(expected));
    }

    #[test]
    fn writes_the_way_networkmanager_does() {
        let address = MacAddress::from([0x00, 0x0b, 0xff]);
        assert_eq!(address.to_string(), "00:0B:FF");
        assert_eq!(format!("{address:?}"), "MacAddress(00:0B:FF)");
    }

    #[test]
    fn keeps_the_length_it_was_given() {
        // An IPoIB hardware address is twenty octets, and NetworkManager uses the same field.
        let ipoib: MacAddress = vec![0xa0; 20].into();
        assert_eq!(ipoib.len(), 20);
        assert_eq!(ipoib.to_string().parse(), Ok(ipoib));
    }

    #[test]
    fn refuses_what_is_not_an_address() {
        for text in [
            "",
            "4C:79:6E:DA:29:4",
            "zz:79:6e:da:29:47",
            "4C79.6EDA.2947",
        ] {
            assert!(text.parse::<MacAddress>().is_err(), "{text} parsed");
        }
        let error = "wat".parse::<MacAddress>().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`wat` is not a hardware address")
        );
    }

    #[test]
    fn round_trips_through_a_variant() {
        let address = MacAddress::from([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47]);
        let value = zbus::zvariant::Value::from(address.clone());
        assert_eq!(value.value_signature(), "ay");
        assert_eq!(MacAddress::try_from(value).unwrap(), address);
    }

    #[test]
    fn assigned_reads_the_special_values_and_addresses_alike() {
        use AssignedMacAddress as Assigned;
        for (text, expected) in [
            ("preserve", Assigned::Preserve),
            ("permanent", Assigned::Permanent),
            ("random", Assigned::Random),
            ("stable", Assigned::Stable),
            ("stable-ssid", Assigned::StableSsid),
            (
                "4C:79:6E:DA:29:47",
                Assigned::Address(MacAddress::from([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x47])),
            ),
        ] {
            assert_eq!(Assigned::from(text), expected, "reading {text}");
            assert_eq!(expected.to_string(), text, "writing {text}");
        }
    }

    #[test]
    fn assigned_keeps_what_it_cannot_read() {
        // Whatever a newer NetworkManager grows has to survive being read and written back.
        let value = AssignedMacAddress::from("stable-per-lunar-phase");
        assert_eq!(
            value,
            AssignedMacAddress::Other("stable-per-lunar-phase".to_owned())
        );
        assert!(value.is_unknown());
        assert_eq!(value.to_string(), "stable-per-lunar-phase");
        assert_eq!(value.address(), None);
    }

    #[test]
    fn assigned_round_trips_through_a_variant() {
        let assigned = AssignedMacAddress::from("4c:79:6e:da:29:47");
        let value = zbus::zvariant::Value::from(assigned.clone());
        assert_eq!(value.value_signature(), "s");
        assert_eq!(AssignedMacAddress::try_from(value).unwrap(), assigned);
        // Written in the canonical form, which is the one NetworkManager sends.
        assert_eq!(assigned.to_string(), "4C:79:6E:DA:29:47");
    }
}
