//! Hand-written type annotations for the members upstream does not annotate readably.
//!
//! NetworkManager states most types in its documentation, but not all of them, and not always
//! in a form worth guessing at — `VersionInfo` is an array whose elements do not all mean the
//! same thing, yet its text names a single type. `annotations.toml` at the workspace root is
//! where those gaps are filled in by hand.
//!
//! Entries are data, not exceptions: every one is checked against the vendored spec, so an
//! annotation that names a member, an interface or a type that no longer exists fails
//! generation rather than being ignored.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::annotate::{Docs, TypeRef};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotations {
    #[serde(default)]
    property: Vec<Property>,
    #[serde(default)]
    argument: Vec<Argument>,
    #[serde(default)]
    setting: Vec<SettingName>,
    #[serde(default)]
    object: Vec<ObjectTarget>,
    #[serde(default)]
    device: Vec<DeviceInterface>,
    #[serde(default)]
    vardict: Vec<Vardict>,
    #[serde(default)]
    enumeration: Vec<Enumeration>,
    #[serde(default)]
    newtype: Vec<Newtype>,
}

/// The values a settings property takes, where NetworkManager declares no constants for them.
///
/// Checked against the settings table: the property must exist and travel as a string, and it
/// must not be one whose values [`crate::values::derive`] already reads out of the spec.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enumeration {
    /// The setting, as it appears in a profile, e.g. `802-11-wireless`.
    pub setting: String,
    /// The property, e.g. `band`.
    pub property: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default, rename = "value")]
    pub values: Vec<EnumerationValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumerationValue {
    /// The string that travels, e.g. `bg`.
    pub value: String,
    /// The variant to generate, where the value itself would not make a readable one.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
}

/// A `byte array` property read through one of the hand-written types in `nm-automata`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Newtype {
    pub setting: String,
    pub property: String,
    /// The type to give it, e.g. `Ssid`.
    #[serde(rename = "type")]
    pub ty: String,
}

/// A dictionary property whose keys NetworkManager documents only in prose.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vardict {
    /// The struct to generate, e.g. `AddressData`.
    pub name: String,
    pub doc: String,
    /// The properties it describes, as `interface.Property`.
    pub applies: Vec<String>,
    pub keys: Vec<VardictKey>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VardictKey {
    /// The key as it appears in the dictionary, e.g. `next-hop`.
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub doc: String,
}

/// The interface a device type implies, where it is not named after it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceInterface {
    /// The introspection name of the `NMDeviceType` value, e.g. `ethernet`.
    #[serde(rename = "type")]
    ty: String,
    interface: String,
}

/// What kind of object an object-path member points at, where it cannot be worked out.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectTarget {
    interface: String,
    /// A property name, or `Member.argument` for an argument.
    name: String,
    /// The interface an object at that path implements.
    target: String,
}

/// A name for a setting whose own name cannot open a Rust identifier.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingName {
    name: String,
    field: String,
}

impl Annotations {
    /// The hand-written dictionary shapes.
    pub fn vardicts(&self) -> &[Vardict] {
        &self.vardict
    }

    /// The hand-written value sets, for the properties whose values are only in the prose.
    pub fn enumerations(&self) -> &[Enumeration] {
        &self.enumeration
    }

    /// The properties given one of the crate's own types.
    pub fn newtypes(&self) -> &[Newtype] {
        &self.newtype
    }

    /// The hand-given device-type interfaces, keyed by the type's introspection name.
    pub fn device_interfaces(&self) -> Vec<(String, String)> {
        self.device
            .iter()
            .map(|d| (d.ty.clone(), d.interface.clone()))
            .collect()
    }

    /// The hand-given object-path targets, as [`crate::objects::resolve`] wants them.
    pub fn object_targets(&self) -> Vec<(String, String, String)> {
        self.object
            .iter()
            .map(|o| (o.interface.clone(), o.name.clone(), o.target.clone()))
            .collect()
    }

    /// The hand-given field names for settings, as [`crate::settings::parse`] wants them.
    pub fn setting_fields(&self) -> Vec<(String, String)> {
        self.setting
            .iter()
            .map(|s| (s.name.clone(), s.field.clone()))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Property {
    interface: String,
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Argument {
    interface: String,
    member: String,
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

/// Read `annotations.toml`, or return an empty set if there is none.
pub fn load(root: &Path) -> Result<Annotations> {
    let path = root.join("annotations.toml");
    let Ok(source) = std::fs::read_to_string(&path) else {
        return Ok(Annotations::default());
    };
    toml::from_str(&source).with_context(|| format!("parsing {}", path.display()))
}

impl Annotations {
    /// Apply the annotations to the documentation recovered from the spec.
    ///
    /// `docs` covers one introspection file, so entries for other files are skipped here and
    /// accounted for by [`Annotations::check_all_applied`].
    pub fn apply(&self, docs: &mut Docs, known_types: &HashSet<String>) -> Result<()> {
        for property in &self.property {
            let Some(interface) = docs.interfaces.get_mut(&property.interface) else {
                continue;
            };
            check_type(&property.ty, known_types)?;
            interface
                .members
                .entry(property.name.clone())
                .or_default()
                .returns = Some(TypeRef {
                c_name: property.ty.clone(),
                from_prose: false,
            });
        }

        for argument in &self.argument {
            let Some(interface) = docs.interfaces.get_mut(&argument.interface) else {
                continue;
            };
            check_type(&argument.ty, known_types)?;
            interface
                .members
                .entry(argument.member.clone())
                .or_default()
                .args
                .entry(argument.name.clone())
                .or_default()
                .ty = Some(TypeRef {
                c_name: argument.ty.clone(),
                from_prose: false,
            });
        }

        Ok(())
    }

    /// Fail if an annotation names an interface that the vendored spec does not have.
    ///
    /// Whether the *member* exists is settled later: an annotation whose signature does not
    /// match is dropped by [`crate::render`], and `check_all_applied` sees only what survived.
    pub fn check_all_applied(&self, applied: &HashSet<(String, String)>) -> Result<()> {
        for property in &self.property {
            if !applied.contains(&(property.interface.clone(), property.name.clone())) {
                bail!(
                    "annotations.toml annotates {}.{} as {}, but the vendored spec has no such \
                     property with that signature",
                    property.interface,
                    property.name,
                    property.ty
                );
            }
        }
        for argument in &self.argument {
            let member = format!("{}.{}", argument.member, argument.name);
            if !applied.contains(&(argument.interface.clone(), member.clone())) {
                bail!(
                    "annotations.toml annotates {}.{} as {}, but the vendored spec has no such \
                     argument with that signature",
                    argument.interface,
                    member,
                    argument.ty
                );
            }
        }
        Ok(())
    }
}

fn check_type(name: &str, known_types: &HashSet<String>) -> Result<()> {
    if !known_types.contains(name) {
        bail!("annotations.toml names `{name}`, which is not in the vendored headers");
    }
    Ok(())
}
