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

#[cfg(test)]
mod tests {
    use super::MacAddress;

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
}
