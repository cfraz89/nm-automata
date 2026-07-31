//! The wire representation shared by every generated NetworkManager type.
//!
//! NetworkManager sends enumerations and flag sets as bare integers, and adds values to them
//! between releases. Both macros here therefore decode anything: an unfamiliar enumeration
//! value becomes `Other(..)` and unfamiliar bits are retained, so a binding generated against
//! an older release keeps working against a newer daemon instead of failing to decode.

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

/// The D-Bus plumbing both macros need: a type that is (de)serialised as its integer.
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

pub(crate) use {nm_enum, nm_flags, nm_wire_repr};
