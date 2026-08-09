//! NetworkManager's own version, together with the capabilities it can be queried for.

use crate::VersionInfoCapability;

/// The `VersionInfo` D-Bus property, decoded.
///
/// NetworkManager packs both into one array of integers: the first element is its own version,
/// and the rest are a bitfield of capabilities spread across as many further elements as it
/// takes — wide enough that a capability past bit 31 just grows the array, rather than needing a
/// new property.
///
/// ```
/// # use nm_automata::{VersionInfo, VersionInfoCapability};
/// let info = VersionInfo::from_wire(vec![0x01_38_02, 0b101]);
/// assert_eq!(info.nm_version, 0x01_38_02);
/// assert!(info.capabilities.contains(&VersionInfoCapability::SyncRouteWithTable));
/// assert!(!info.capabilities.contains(&VersionInfoCapability::Ip4Forwarding));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// `major << 16 | minor << 8 | micro`, comparable numerically, as NetworkManager's own
    /// documentation states.
    pub nm_version: u32,
    /// The capabilities the daemon supports.
    pub capabilities: Vec<VersionInfoCapability>,
}

impl VersionInfo {
    /// Decode the array NetworkManager sends.
    pub fn from_wire(words: Vec<u32>) -> Self {
        let mut words = words.into_iter();
        let nm_version = words.next().unwrap_or(0);
        let capabilities = words
            .enumerate()
            .flat_map(|(word, bits)| {
                (0..u32::BITS)
                    .filter(move |bit| bits & (1 << bit) != 0)
                    .map(move |bit| VersionInfoCapability::from_wire(word as u32 * u32::BITS + bit))
            })
            .collect();
        Self {
            nm_version,
            capabilities,
        }
    }

    /// Encode back to the array NetworkManager sends.
    fn to_wire(&self) -> Vec<u32> {
        let mut words = vec![self.nm_version];
        for capability in &self.capabilities {
            let index = capability.bits();
            let word = (index / u32::BITS) as usize;
            if words.len() <= word + 1 {
                words.resize(word + 2, 0);
            }
            words[word + 1] |= 1 << (index % u32::BITS);
        }
        words
    }
}

impl zbus::zvariant::Type for VersionInfo {
    const SIGNATURE: &'static zbus::zvariant::Signature =
        <Vec<u32> as zbus::zvariant::Type>::SIGNATURE;
}

impl serde::Serialize for VersionInfo {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.to_wire(), serializer)
    }
}

impl<'de> serde::Deserialize<'de> for VersionInfo {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <Vec<u32> as serde::Deserialize>::deserialize(deserializer).map(Self::from_wire)
    }
}

impl From<VersionInfo> for zbus::zvariant::Value<'_> {
    fn from(value: VersionInfo) -> Self {
        zbus::zvariant::Value::from(value.to_wire())
    }
}

impl TryFrom<zbus::zvariant::OwnedValue> for VersionInfo {
    type Error = zbus::zvariant::Error;

    fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {
        Vec::<u32>::try_from(value).map(Self::from_wire)
    }
}

impl TryFrom<zbus::zvariant::Value<'_>> for VersionInfo {
    type Error = zbus::zvariant::Error;

    fn try_from(value: zbus::zvariant::Value<'_>) -> Result<Self, Self::Error> {
        Vec::<u32>::try_from(value).map(Self::from_wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_version_and_the_bits_that_are_set() {
        let info = VersionInfo::from_wire(vec![70658, 0b101, 0b1]);
        assert_eq!(info.nm_version, 70658);
        assert_eq!(
            info.capabilities,
            vec![
                VersionInfoCapability::SyncRouteWithTable,
                VersionInfoCapability::SriovPreserveOnDown,
                VersionInfoCapability::from_wire(32),
            ]
        );
    }

    #[test]
    fn an_absent_array_is_version_zero_with_no_capabilities() {
        let info = VersionInfo::from_wire(vec![]);
        assert_eq!(info.nm_version, 0);
        assert!(info.capabilities.is_empty());
    }

    #[test]
    fn round_trips_through_a_variant() {
        let info = VersionInfo::from_wire(vec![70658, 0b101, 0b1]);
        let value = zbus::zvariant::Value::from(info.clone());
        assert_eq!(value.value_signature(), "au");
        assert_eq!(VersionInfo::try_from(value).unwrap(), info);
    }

    /// Round-tripping a struct built directly, rather than decoded, exercises encoding a
    /// capability whose bit sits in a later word than any before it.
    #[test]
    fn round_trips_a_capability_that_grows_the_word_array() {
        let info = VersionInfo {
            nm_version: 1,
            capabilities: vec![VersionInfoCapability::from_wire(65)],
        };
        assert_eq!(VersionInfo::from_wire(info.to_wire()), info);
    }
}
