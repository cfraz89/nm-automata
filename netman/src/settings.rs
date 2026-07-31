//! Connection profiles: the typed side of NetworkManager's `a{sa{sv}}`.
//!
//! A profile is a map of setting name to a map of property name to variant, which is the least
//! typed thing in NetworkManager's D-Bus API. [`crate::ConnectionProfile`] and the generated
//! setting structs give it real fields, and this module is the plumbing they are built on.
//!
//! # Absent is not the same as default
//!
//! Every property is an `Option`. That is not timidity: NetworkManager normalises a profile
//! before validating it, and an absent property means "decide for me" while a present one
//! means "use exactly this". `connection.autoconnect-retries` left unset consults the global
//! default; set to `0` it means *forever*. Writing back a profile with the defaults filled in
//! would quietly change what it means.
//!
//! # Unknown settings and properties are preserved
//!
//! Anything this binding does not know about is kept in the `other` maps of
//! [`ConnectionProfile`](crate::ConnectionProfile) and of each setting, so reading a profile
//! from a newer NetworkManager, changing one field and writing it back does not drop the rest.

use std::collections::HashMap;
use std::fmt;

use zbus::zvariant::{OwnedValue, Value};

/// The properties of one setting, as NetworkManager sends them.
pub type SettingMap = HashMap<String, OwnedValue>;

/// A whole connection profile, as NetworkManager sends it (`a{sa{sv}}`).
pub type ProfileMap = HashMap<String, SettingMap>;

/// A property that could not be converted between its Rust type and the wire.
#[derive(Debug)]
pub struct SettingsError {
    setting: &'static str,
    property: String,
    message: String,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}: {}", self.setting, self.property, self.message)
    }
}

impl std::error::Error for SettingsError {}

impl From<SettingsError> for zbus::Error {
    fn from(error: SettingsError) -> Self {
        zbus::Error::Failure(error.to_string())
    }
}

/// One setting of a connection profile.
pub trait Setting: Sized {
    /// The key this setting appears under in a profile, e.g. `802-11-wireless`.
    const NAME: &'static str;

    /// The property names this binding knows; anything else is kept aside.
    const PROPERTIES: &'static [&'static str];

    fn from_map(map: &SettingMap) -> Result<Self, SettingsError>;

    fn to_map(&self) -> Result<SettingMap, SettingsError>;
}

/// Read one property, leaving it unset if NetworkManager did not send it.
pub fn get<T>(
    map: &SettingMap,
    setting: &'static str,
    property: &'static str,
) -> Result<Option<T>, SettingsError>
where
    T: TryFrom<OwnedValue>,
    <T as TryFrom<OwnedValue>>::Error: fmt::Display,
{
    let Some(value) = map.get(property) else {
        return Ok(None);
    };
    let value = value.try_clone().map_err(|e| SettingsError {
        setting,
        property: property.to_owned(),
        message: e.to_string(),
    })?;
    T::try_from(value).map(Some).map_err(|e| SettingsError {
        setting,
        property: property.to_owned(),
        message: format!("cannot read as {}: {e}", std::any::type_name::<T>()),
    })
}

/// Write one property, omitting it entirely when unset.
pub fn set<'a, T>(
    map: &mut SettingMap,
    setting: &'static str,
    property: &'static str,
    value: Option<T>,
) -> Result<(), SettingsError>
where
    T: Into<Value<'a>>,
{
    let Some(value) = value else {
        return Ok(());
    };
    let value = OwnedValue::try_from(value.into()).map_err(|e| SettingsError {
        setting,
        property: property.to_owned(),
        message: e.to_string(),
    })?;
    map.insert(property.to_owned(), value);
    Ok(())
}

/// The entries of `map` this binding has no field for.
pub fn unknown(
    map: &SettingMap,
    setting: &'static str,
    known: &[&str],
) -> Result<SettingMap, SettingsError> {
    map.iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| {
            let value = value.try_clone().map_err(|e| SettingsError {
                setting,
                property: key.clone(),
                message: e.to_string(),
            })?;
            Ok((key.clone(), value))
        })
        .collect()
}

/// Copy the properties kept aside by [`unknown`] back into a setting being written.
pub fn merge_unknown(
    map: &mut SettingMap,
    setting: &'static str,
    other: &SettingMap,
) -> Result<(), SettingsError> {
    for (key, value) in other {
        let value = value.try_clone().map_err(|e| SettingsError {
            setting,
            property: key.clone(),
            message: e.to_string(),
        })?;
        map.insert(key.clone(), value);
    }
    Ok(())
}

/// Read a setting out of a profile, if it is present.
pub fn setting_from<S: Setting>(profile: &ProfileMap) -> Result<Option<S>, SettingsError> {
    profile.get(S::NAME).map(S::from_map).transpose()
}

/// Write a setting into a profile, if it is present.
pub fn setting_into<S: Setting>(
    profile: &mut ProfileMap,
    setting: Option<&S>,
) -> Result<(), SettingsError> {
    if let Some(setting) = setting {
        profile.insert(S::NAME.to_owned(), setting.to_map()?);
    }
    Ok(())
}
