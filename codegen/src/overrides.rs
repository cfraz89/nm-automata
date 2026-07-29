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
