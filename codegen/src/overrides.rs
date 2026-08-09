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

/// The values a string takes, where NetworkManager declares no constants for them.
///
/// Either the values of a connection-profile property, named by its `setting` and `property` —
/// checked against the settings table, which must type it as a string and must not be one whose
/// values [`crate::values::derive`] already reads out of the spec — or, given a `name` instead,
/// a type standing on its own, for a [`Vardict`] key to be typed as.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enumeration {
    /// The setting, as it appears in a profile, e.g. `802-11-wireless`.
    #[serde(default)]
    setting: Option<String>,
    /// The property, e.g. `band`.
    #[serde(default)]
    property: Option<String>,
    /// The type to generate, where the values are not those of a profile property.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default, rename = "value")]
    pub values: Vec<EnumerationValue>,
}

impl Enumeration {
    /// The profile property whose values these are, if that is the form it takes.
    pub fn profile_property(&self) -> Option<(&str, &str)> {
        Some((self.setting.as_deref()?, self.property.as_deref()?))
    }

    /// The name of the type to generate, where it is not named after a property.
    pub fn type_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// One naming a connection-profile property, for the tests.
    #[cfg(test)]
    pub fn property(setting: &str, property: &str, values: Vec<EnumerationValue>) -> Self {
        Self {
            setting: Some(setting.to_owned()),
            property: Some(property.to_owned()),
            name: None,
            doc: None,
            values,
        }
    }

    /// How it reads in an error message.
    pub fn describe(&self) -> String {
        match (self.profile_property(), self.type_name()) {
            (Some((setting, property)), _) => format!("{setting}.{property}"),
            (_, Some(name)) => name.to_owned(),
            _ => "an `[[enumeration]]` entry".to_owned(),
        }
    }

    /// Fail on an entry that names neither form, or both, or half of one.
    fn check(&self) -> Result<()> {
        let named = [
            ("setting", self.setting.is_some()),
            ("property", self.property.is_some()),
            ("name", self.name.is_some()),
        ];
        let given: Vec<&str> = named
            .iter()
            .filter_map(|(field, given)| given.then_some(*field))
            .collect();
        if !matches!(given[..], ["setting", "property"] | ["name"]) {
            bail!(
                "an `[[enumeration]]` entry gives {}, but it must name either a profile property \
                 (`setting` and `property`) or a type of its own (`name`)",
                match given.is_empty() {
                    true => "neither".to_owned(),
                    false => format!("`{}`", given.join("`, `")),
                }
            );
        }
        if self.values.is_empty() {
            bail!(
                "the `[[enumeration]]` entry for `{}` has no values",
                self.describe()
            );
        }
        Ok(())
    }
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

/// A member read through one of the hand-written types in `nm-automata`.
///
/// Either a connection-profile property, named by its `setting` and `property`, or a member of
/// a D-Bus interface, named by its `interface` and `name`. Both are checked against the spec:
/// the member has to exist, and to travel as what the type reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Newtype {
    #[serde(default)]
    setting: Option<String>,
    #[serde(default)]
    property: Option<String>,
    #[serde(default)]
    interface: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// The type to give it, e.g. `Ssid`.
    #[serde(rename = "type")]
    pub ty: String,
}

impl Newtype {
    /// The connection-profile property this applies to, if that is the form it takes.
    pub fn profile_property(&self) -> Option<(&str, &str)> {
        Some((self.setting.as_deref()?, self.property.as_deref()?))
    }

    /// The interface member this applies to, if that is the form it takes.
    pub fn interface_member(&self) -> Option<(&str, &str)> {
        Some((self.interface.as_deref()?, self.name.as_deref()?))
    }

    /// One naming a connection-profile property, for the tests.
    #[cfg(test)]
    pub fn profile(setting: &str, property: &str, ty: &str) -> Self {
        Self {
            setting: Some(setting.to_owned()),
            property: Some(property.to_owned()),
            interface: None,
            name: None,
            ty: ty.to_owned(),
        }
    }

    /// The member as it reads in an error message.
    pub fn describe(&self) -> String {
        match (self.profile_property(), self.interface_member()) {
            (Some((setting, property)), _) => format!("{setting}.{property}"),
            (_, Some((interface, name))) => format!("{interface}.{name}"),
            _ => format!("a `[[newtype]]` entry for `{}`", self.ty),
        }
    }

    /// Fail on an entry that names neither form, or both, or half of one.
    fn check(&self) -> Result<()> {
        let fields = [&self.setting, &self.property, &self.interface, &self.name];
        let given: Vec<&str> = ["setting", "property", "interface", "name"]
            .into_iter()
            .zip(fields)
            .filter_map(|(name, value)| value.is_some().then_some(name))
            .collect();
        if !matches!(given[..], ["setting", "property"] | ["interface", "name"]) {
            bail!(
                "the `[[newtype]]` entry for `{}` gives {}, but it must name either a profile \
                 property (`setting` and `property`) or an interface member (`interface` and \
                 `name`)",
                self.ty,
                match given.is_empty() {
                    true => "neither".to_owned(),
                    false => format!("`{}`", given.join("`, `")),
                }
            );
        }
        Ok(())
    }
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

    /// Fail if a `[[vardict]]` applies to a member the spec does not have, or to one that does
    /// not carry a dictionary.
    ///
    /// `sites` is every member that could, with the signature it has — see
    /// [`crate::render::dictionary_sites`].
    pub fn check_vardicts(&self, sites: &[(String, String)]) -> Result<()> {
        for vardict in &self.vardict {
            for target in &vardict.applies {
                let Some((_, signature)) = sites.iter().find(|(member, _)| member == target) else {
                    bail!(
                        "annotations.toml gives `{target}` the shape `{}`, but the vendored spec \
                         has no such member",
                        vardict.name
                    );
                };
                if !matches!(
                    signature.as_str(),
                    crate::render::VARDICT_SIGNATURE | crate::render::VARDICT_ARRAY_SIGNATURE
                ) {
                    bail!(
                        "annotations.toml gives `{target}` the shape `{}`, but it travels as \
                         `{signature}` rather than as a dictionary",
                        vardict.name
                    );
                }
            }
        }
        Ok(())
    }

    /// The members given one of the crate's own types.
    pub fn newtypes(&self) -> &[Newtype] {
        &self.newtype
    }

    /// The interface members among them, as `(interface, member, type)`.
    pub fn member_newtypes(&self) -> Vec<(&str, &str, &str)> {
        self.newtype
            .iter()
            .filter_map(|entry| {
                let (interface, name) = entry.interface_member()?;
                Some((interface, name, entry.ty.as_str()))
            })
            .collect()
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
    let annotations: Annotations =
        toml::from_str(&source).with_context(|| format!("parsing {}", path.display()))?;
    for newtype in &annotations.newtype {
        newtype.check()?;
    }
    for enumeration in &annotations.enumeration {
        enumeration.check()?;
    }
    Ok(annotations)
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
