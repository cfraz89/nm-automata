//! The name of a Wi-Fi network.

use std::borrow::Cow;
use std::fmt;

use crate::repr::nm_bytes_repr;

/// The name of a Wi-Fi network, as 802.11 defines it: up to 32 arbitrary octets.
///
/// An SSID is deliberately not a string. The standard puts no encoding on those octets, and
/// networks named in a local encoding — or with a trailing NUL, or with no printable characters
/// at all — are common enough that decoding one as UTF-8 and storing the result would lose the
/// name of the network it came from. So this keeps the bytes NetworkManager sent, and offers
/// text as a reading of them:
///
/// ```
/// # use nm_automata::Ssid;
/// let ssid = Ssid::from("café wifi");
/// assert_eq!(ssid.to_str(), Some("café wifi"));
/// assert_eq!(ssid.as_bytes(), "café wifi".as_bytes());
///
/// // A name in some other encoding is still an SSID, and still round-trips.
/// let latin1 = Ssid::from(b"caf\xe9 wifi".as_slice());
/// assert_eq!(latin1.to_str(), None);
/// assert_eq!(latin1.to_string_lossy(), "caf� wifi");
/// ```
///
/// Comparison is over the bytes, so two SSIDs are equal when NetworkManager would treat them as
/// the same network.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ssid(Vec<u8>);

impl Ssid {
    /// The SSID these bytes name.
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

    /// The name as text, if the octets are valid UTF-8.
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    /// The name as text, with anything that is not valid UTF-8 replaced by `U+FFFD`.
    ///
    /// For display only: the result no longer names the network it came from.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    /// How many octets the name is.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the name is empty, which for a profile means "any network".
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Lossily, as [`to_string_lossy`](Ssid::to_string_lossy).
impl fmt::Display for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

/// As the text when that is what the octets are, and as the octets when it is not — so that a
/// name which is not UTF-8 is still readable in a debug dump rather than being papered over.
impl fmt::Debug for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_str() {
            Some(text) => write!(f, "Ssid({text:?})"),
            None => write!(f, "Ssid({:02X?})", self.0),
        }
    }
}

impl From<&str> for Ssid {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

impl From<String> for Ssid {
    fn from(text: String) -> Self {
        Self(text.into_bytes())
    }
}

impl<const N: usize> From<&[u8; N]> for Ssid {
    fn from(bytes: &[u8; N]) -> Self {
        Self(bytes.to_vec())
    }
}

nm_bytes_repr!(Ssid);

#[cfg(test)]
mod tests {
    use super::Ssid;

    #[test]
    fn keeps_the_octets_it_was_given() {
        assert_eq!(Ssid::from("home wifi").as_bytes(), b"home wifi");
        assert_eq!(Ssid::from(b"\x00\xff").as_bytes(), b"\x00\xff");
        assert_eq!(Ssid::from("home wifi"), Ssid::from(b"home wifi"));
    }

    #[test]
    fn reads_as_text_only_when_it_is_text() {
        assert_eq!(Ssid::from("café").to_str(), Some("café"));
        assert_eq!(Ssid::from(b"caf\xe9").to_str(), None);
        assert_eq!(Ssid::from(b"caf\xe9").to_string_lossy(), "caf\u{fffd}");
    }

    #[test]
    fn shows_bytes_when_it_is_not_text() {
        assert_eq!(format!("{:?}", Ssid::from("hi")), r#"Ssid("hi")"#);
        assert_eq!(format!("{:?}", Ssid::from(b"\xe9!")), "Ssid([E9, 21])");
    }

    #[test]
    fn round_trips_through_a_variant() {
        let ssid = Ssid::from(b"caf\xe9 wifi");
        let value = zbus::zvariant::Value::from(ssid.clone());
        assert_eq!(value.value_signature(), "ay");
        assert_eq!(Ssid::try_from(value).unwrap(), ssid);
    }
}
