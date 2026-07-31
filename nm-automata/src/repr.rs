//! The wire representation shared by every generated NetworkManager type.
//!
//! NetworkManager sends enumerations and flag sets as bare integers, and adds values to them
//! between releases. Every macro here therefore decodes anything: an unfamiliar enumeration
//! value becomes `Other(..)` and unfamiliar bits are retained, so a binding generated against
//! an older release keeps working against a newer daemon instead of failing to decode.
//!
//! A few connection-profile properties enumerate their values as strings rather than as
//! integers — `802-11-wireless.mode` is one — and [`nm_str_enum`] gives those the same
//! treatment, down to keeping an unrecognised value verbatim so that writing the profile back
//! sends exactly what was read.

/// Define an enumeration that travels as an integer.
macro_rules! nm_enum {
    (
        $(#$meta:tt)*
        pub enum $name:ident: $repr:ty {
            $(
                $(#$variant_meta:tt)*
                $variant:ident = $value:literal,
            )*
        }
    ) => {
        $(#$meta)*
        ///
        /// Values added to NetworkManager after this binding was generated decode to
        /// [`Other`](Self::Other) rather than failing.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $(
                $(#$variant_meta)*
                $variant,
            )*
            /// A value this binding does not know about.
            Other($repr),
        }

        impl $name {
            /// The integer NetworkManager represents this value with.
            pub const fn bits(self) -> $repr {
                match self {
                    $( Self::$variant => $value, )*
                    Self::Other(bits) => bits,
                }
            }

            /// Whether this is a value the binding does not recognise.
            pub const fn is_unknown(self) -> bool {
                matches!(self, Self::Other(_))
            }

            /// Decode the integer NetworkManager sent.
            pub const fn from_wire(bits: $repr) -> Self {
                match bits {
                    $( $value => Self::$variant, )*
                    other => Self::Other(other),
                }
            }
        }

        impl From<$repr> for $name {
            fn from(bits: $repr) -> Self {
                Self::from_wire(bits)
            }
        }

        impl From<$name> for $repr {
            fn from(value: $name) -> Self {
                value.bits()
            }
        }

        $crate::repr::nm_wire_repr!($name, $repr);
    };
}

/// Define a set of independent bits that travels as an integer.
macro_rules! nm_flags {
    (
        $(#$meta:tt)*
        pub enum $name:ident: $repr:ty {
            $(
                $(#$variant_meta:tt)*
                $variant:ident = $value:literal,
            )*
        }
    ) => {
        bitflags::bitflags! {
            $(#$meta)*
            ///
            /// Bits added to NetworkManager after this binding was generated are kept rather
            /// than rejected.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $name: $repr {
                $(
                    $(#$variant_meta)*
                    const $variant = $value;
                )*
            }
        }

        impl $name {
            /// Decode the integer NetworkManager sent.
            pub const fn from_wire(bits: $repr) -> Self {
                Self::from_bits_retain(bits)
            }
        }

        impl From<$repr> for $name {
            fn from(bits: $repr) -> Self {
                Self::from_wire(bits)
            }
        }

        impl From<$name> for $repr {
            fn from(value: $name) -> Self {
                value.bits()
            }
        }

        $crate::repr::nm_wire_repr!($name, $repr);
    };
}

/// Define an enumeration that travels as a string.
///
/// Unlike the integer enumerations, an unrecognised value has to be *kept*, not just tolerated:
/// these are connection-profile properties, and a profile is read, changed and written back, so
/// a value this binding does not know must survive the round trip unaltered.
macro_rules! nm_str_enum {
    (
        $(#$meta:tt)*
        pub enum $name:ident {
            $(
                $(#$variant_meta:tt)*
                $variant:ident = $value:literal,
            )*
        }
    ) => {
        $(#$meta)*
        ///
        /// Values added to NetworkManager after this binding was generated decode to
        /// [`Other`](Self::Other) rather than failing, and are written back as they came.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $(
                $(#$variant_meta)*
                $variant,
            )*
            /// A value this binding does not know about.
            Other(String),
        }

        impl $name {
            /// The string NetworkManager represents this value with.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $value, )*
                    Self::Other(text) => text,
                }
            }

            /// Whether this is a value the binding does not recognise.
            pub fn is_unknown(&self) -> bool {
                matches!(self, Self::Other(_))
            }

            /// Decode the string NetworkManager sent.
            pub fn from_wire(text: impl Into<String>) -> Self {
                let text = text.into();
                match text.as_str() {
                    $( $value => Self::$variant, )*
                    _ => Self::Other(text),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        // Infallible, because any string is a value: the ones this binding does not know are
        // exactly what `Other` is for.
        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Ok(Self::from_wire(text))
            }
        }

        impl From<&str> for $name {
            fn from(text: &str) -> Self {
                Self::from_wire(text)
            }
        }

        impl From<String> for $name {
            fn from(text: String) -> Self {
                Self::from_wire(text)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                match value {
                    $( $name::$variant => $value.to_owned(), )*
                    $name::Other(text) => text,
                }
            }
        }

        impl zbus::zvariant::Type for $name {
            const SIGNATURE: &'static zbus::zvariant::Signature =
                <String as zbus::zvariant::Type>::SIGNATURE;
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <String as serde::Deserialize>::deserialize(deserializer).map(Self::from_wire)
            }
        }

        impl From<$name> for zbus::zvariant::Value<'_> {
            fn from(value: $name) -> Self {
                zbus::zvariant::Value::from(String::from(value))
            }
        }

        impl TryFrom<zbus::zvariant::OwnedValue> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {
                String::try_from(value).map(Self::from_wire)
            }
        }

        impl TryFrom<zbus::zvariant::Value<'_>> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::Value<'_>) -> Result<Self, Self::Error> {
                String::try_from(value).map(Self::from_wire)
            }
        }
    };
}

/// The D-Bus plumbing for a newtype over the bytes NetworkManager sends.
///
/// The wire form is unchanged — `ay`, exactly as the property is declared — so these are a
/// reading of the same bytes and never a reinterpretation of them.
macro_rules! nm_bytes_repr {
    ($name:ident) => {
        impl AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl From<Vec<u8>> for $name {
            fn from(bytes: Vec<u8>) -> Self {
                Self(bytes)
            }
        }

        impl From<&[u8]> for $name {
            fn from(bytes: &[u8]) -> Self {
                Self(bytes.to_vec())
            }
        }

        impl From<$name> for Vec<u8> {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl zbus::zvariant::Type for $name {
            const SIGNATURE: &'static zbus::zvariant::Signature =
                <Vec<u8> as zbus::zvariant::Type>::SIGNATURE;
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serde::Serialize::serialize(&self.0, serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <Vec<u8> as serde::Deserialize>::deserialize(deserializer).map(Self)
            }
        }

        impl From<$name> for zbus::zvariant::Value<'_> {
            fn from(value: $name) -> Self {
                zbus::zvariant::Value::from(value.0)
            }
        }

        impl TryFrom<zbus::zvariant::OwnedValue> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {
                Vec::<u8>::try_from(value).map(Self)
            }
        }

        impl TryFrom<zbus::zvariant::Value<'_>> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::Value<'_>) -> Result<Self, Self::Error> {
                Vec::<u8>::try_from(value).map(Self)
            }
        }
    };
}

/// The D-Bus plumbing the integer macros need: a type that is (de)serialised as its integer.
macro_rules! nm_wire_repr {
    ($name:ident, $repr:ty) => {
        impl zbus::zvariant::Type for $name {
            const SIGNATURE: &'static zbus::zvariant::Signature =
                <$repr as zbus::zvariant::Type>::SIGNATURE;
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serde::Serialize::serialize(&self.bits(), serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <$repr as serde::Deserialize>::deserialize(deserializer).map(Self::from_wire)
            }
        }

        impl From<$name> for zbus::zvariant::Value<'_> {
            fn from(value: $name) -> Self {
                zbus::zvariant::Value::from(value.bits())
            }
        }

        impl TryFrom<zbus::zvariant::OwnedValue> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {
                <$repr>::try_from(value).map(Self::from_wire)
            }
        }

        impl TryFrom<zbus::zvariant::Value<'_>> for $name {
            type Error = zbus::zvariant::Error;

            fn try_from(value: zbus::zvariant::Value<'_>) -> Result<Self, Self::Error> {
                <$repr>::try_from(value).map(Self::from_wire)
            }
        }
    };
}

pub(crate) use {nm_bytes_repr, nm_enum, nm_flags, nm_str_enum, nm_wire_repr};
