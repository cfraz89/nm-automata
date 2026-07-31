//! Emitters: augmented introspection XML, and the Rust module of NetworkManager's types.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use zbus_xml::{ArgDirection, Interface, Node};

use crate::annotate::{Docs, rustdoc_text};
use crate::gir::TypeDef;

/// The Telepathy extensions namespace, whose `tp:` elements carry the names and documentation
/// that plain D-Bus introspection has no room for.
const TP_NS: &str = "http://telepathy.freedesktop.org/wiki/DbusSpec#extensions-v0";

/// A NetworkManager type as it is used on the wire, resolved across the whole spec.
#[derive(Debug, Clone)]
pub struct Resolved<'a> {
    pub def: &'a TypeDef,
    /// The D-Bus signature the type travels as, e.g. `u`.
    pub signature: String,
    /// The Rust integer backing it, e.g. `u32`.
    pub repr: &'static str,
}

/// Rewrite one introspection file with the type and documentation annotations recovered from
/// its comments, in the form `zbus_xmlgen` reads.
pub fn augmented_xml(
    node: &Node<'_>,
    docs: &Docs,
    types: &BTreeMap<String, Resolved<'_>>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(out, r#"<node name="/" xmlns:tp="{TP_NS}">"#);

    // Every type the file's members refer to is defined at node level, where `zbus_xmlgen`
    // picks it up for whichever interfaces reference it.
    for name in referenced_types(node, docs, types) {
        write_type_def(&mut out, &types[&name]);
    }

    for interface in node.interfaces() {
        write_interface(&mut out, interface, docs, types);
    }

    let _ = writeln!(out, "</node>");
    out
}

/// The types referenced by any member of `node`, in a stable order.
fn referenced_types(
    node: &Node<'_>,
    docs: &Docs,
    types: &BTreeMap<String, Resolved<'_>>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for interface in node.interfaces() {
        for (_, candidate) in interface_candidates(interface, docs) {
            if reference_for(&candidate, types).is_some() && !names.contains(&candidate.c_name) {
                names.push(candidate.c_name);
            }
        }
    }
    names.sort();
    names
}

/// Where an annotation applies within an interface.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Site {
    Property(String),
    Arg(String, String),
}

/// A type named by a comment, together with the signature that actually travels at that site.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The name as written in the header, e.g. `NMDeviceState`.
    pub c_name: String,
    /// The D-Bus signature of the property or argument, e.g. `u` or `au`.
    pub signature: String,
}

impl Candidate {
    /// The signature of a single value, with any array wrapper removed.
    pub fn element_signature(&self) -> &str {
        self.signature.strip_prefix('a').unwrap_or(&self.signature)
    }
}

/// Every type reference the comments of `interface` make, before signature checking.
pub fn interface_candidates(interface: &Interface<'_>, docs: &Docs) -> BTreeMap<Site, Candidate> {
    let mut out = BTreeMap::new();
    let Some(iface_docs) = docs.interfaces.get(interface.name().as_str()) else {
        return out;
    };

    for property in interface.properties() {
        let Some(member) = iface_docs.members.get(property.name().as_str()) else {
            continue;
        };
        let signature = property.ty().to_string();
        if let Some(reference) = member.returns.as_ref().filter(|r| r.applies_to(&signature)) {
            out.insert(
                Site::Property(property.name().to_string()),
                Candidate {
                    c_name: reference.c_name.clone(),
                    signature,
                },
            );
        }
    }

    let members = interface
        .methods()
        .iter()
        .map(|m| (m.name().to_string(), m.args()))
        .chain(
            interface
                .signals()
                .iter()
                .map(|s| (s.name().to_string(), s.args())),
        );

    for (name, args) in members {
        let Some(member) = iface_docs.members.get(&name) else {
            continue;
        };
        for arg in args {
            let Some(arg_name) = arg.name() else { continue };
            // A method's single output argument is what `Returns:` describes.
            let annotated = match member.args.get(arg_name).and_then(|a| a.ty.as_ref()) {
                Some(ty) => Some(ty),
                None if arg.direction() == Some(ArgDirection::Out) => member.returns.as_ref(),
                None => None,
            };
            let signature = arg.ty().to_string();
            if let Some(reference) = annotated.filter(|r| r.applies_to(&signature)) {
                out.insert(
                    Site::Arg(name.clone(), arg_name.to_owned()),
                    Candidate {
                        c_name: reference.c_name.clone(),
                        signature,
                    },
                );
            }
        }
    }

    out
}

/// The `tp:type` reference for a site, if the named type really is what travels there.
///
/// A mismatch means the comment and the signature disagree — the annotation is dropped and
/// the member keeps its plain integer type rather than failing to decode at runtime.
fn reference_for(candidate: &Candidate, types: &BTreeMap<String, Resolved<'_>>) -> Option<String> {
    let resolved = types.get(&candidate.c_name)?;
    if candidate.signature == resolved.signature {
        Some(resolved.def.rust_name.clone())
    } else if candidate.signature == format!("a{}", resolved.signature) {
        Some(format!("{}[]", resolved.def.rust_name))
    } else {
        None
    }
}

/// A member that has to reach Rust under a different name than its own.
///
/// NetworkManager gives some interfaces a `State` property *and* a `StateChanged` signal, or a
/// `Managed` property *and* a `SetManaged` method. Both halves of each pair want the same Rust
/// name, so one of them is generated under a sentinel name and renamed afterwards; the
/// property always keeps the plain name, since its accessors are not negotiable.
#[derive(Debug, Clone)]
pub struct Rename {
    /// The member name on the wire, e.g. `StateChanged`.
    pub member: String,
    /// The name the member is generated under, e.g. `netman_rename_0`.
    pub sentinel: String,
    /// The type name generated from the sentinel, e.g. `NetmanRename0`.
    pub sentinel_type: String,
    /// What the generated function should end up called, e.g. `state_changed_signal`.
    pub fn_name: String,
    /// What the generated types should end up called, e.g. `StateChangedSignal`.
    pub type_name: String,
    /// How the member is described in the doc comment `zbus_xmlgen` writes.
    pub kind: &'static str,
}

/// Work out which members of `interface` collide, and what to call them instead.
pub fn plan_renames(interface: &Interface<'_>) -> Vec<Rename> {
    let mut taken: Vec<String> = Vec::new();
    for property in interface.properties() {
        let snake = snake(property.name().as_str());
        taken.push(format!("receive_{snake}_changed"));
        if property.access().write() {
            taken.push(format!("set_{snake}"));
        }
        taken.push(snake);
    }

    let mut renames = Vec::new();
    let mut claim = |wanted: String, member: &str, suffix: &'static str, kind: &'static str| {
        if !taken.contains(&wanted) {
            taken.push(wanted);
            return;
        }
        let index = renames.len();
        renames.push(Rename {
            member: member.to_owned(),
            sentinel: format!("netman_rename_{index}"),
            sentinel_type: format!("NetmanRename{index}"),
            fn_name: format!("{}_{suffix}", snake(member)),
            type_name: format!("{}{}", pascal_case(&snake(member)), capitalise(suffix)),
            kind,
        });
    };

    for method in interface.methods() {
        let name = method.name().to_string();
        claim(snake(&name), &name, "method", "method");
    }
    for signal in interface.signals() {
        let name = signal.name().to_string();
        claim(
            format!("receive_{}", snake(&name)),
            &name,
            "signal",
            "signal",
        );
    }

    renames
}

/// The snake case `zbus_xmlgen` uses for member names.
fn snake(name: &str) -> String {
    snakecase::ascii::to_snakecase(name).into_owned()
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// The annotations of `interface` that survive signature checking.
fn interface_references(
    interface: &Interface<'_>,
    docs: &Docs,
    types: &BTreeMap<String, Resolved<'_>>,
) -> BTreeMap<Site, String> {
    interface_candidates(interface, docs)
        .into_iter()
        .filter_map(|(site, candidate)| {
            reference_for(&candidate, types).map(|reference| (site, reference))
        })
        .collect()
}

fn write_type_def(out: &mut String, resolved: &Resolved<'_>) {
    let def = resolved.def;
    let _ = writeln!(
        out,
        r#"  <tp:enum name="{}" type="{}">"#,
        def.rust_name, resolved.signature
    );
    write_docstring(out, def.doc.as_deref(), "    ");
    // The suffix only has to give `zbus_xmlgen` a valid, distinct identifier for the definition
    // to be usable — the body it generates from this is stripped, and the real type comes from
    // `types_module` below. Using the same names keeps the two readable side by side.
    for (variant, name) in def.rust_variants() {
        let _ = writeln!(
            out,
            r#"    <tp:enumvalue suffix="{name}" value="{}"/>"#,
            variant.value
        );
    }
    let _ = writeln!(out, "  </tp:enum>");
}

fn write_interface(
    out: &mut String,
    interface: &Interface<'_>,
    docs: &Docs,
    types: &BTreeMap<String, Resolved<'_>>,
) {
    let references = interface_references(interface, docs, types);
    let iface_docs = docs.interfaces.get(interface.name().as_str());
    let member_docs = |name: &str| iface_docs.and_then(|i| i.members.get(name));

    let _ = writeln!(out, r#"  <interface name="{}">"#, interface.name());
    write_docstring(out, iface_docs.and_then(|i| i.doc.as_deref()), "    ");

    let renames = plan_renames(interface);
    let emitted_name = |member: &str| {
        renames
            .iter()
            .find(|rename| rename.member == member)
            .map_or(member, |rename| rename.sentinel.as_str())
            .to_owned()
    };

    for method in interface.methods() {
        let name = method.name().to_string();
        let member = member_docs(&name);
        let _ = writeln!(out, r#"    <method name="{}">"#, emitted_name(&name));
        write_docstring(out, member.and_then(|m| m.doc.as_deref()), "      ");
        for arg in method.args() {
            let direction = match arg.direction() {
                Some(ArgDirection::Out) => "out",
                _ => "in",
            };
            write_arg(out, arg, &name, Some(direction), &references, member);
        }
        let _ = writeln!(out, "    </method>");
    }

    for signal in interface.signals() {
        let name = signal.name().to_string();
        let member = member_docs(&name);
        let _ = writeln!(out, r#"    <signal name="{}">"#, emitted_name(&name));
        write_docstring(out, member.and_then(|m| m.doc.as_deref()), "      ");
        for arg in signal.args() {
            write_arg(out, arg, &name, None, &references, member);
        }
        let _ = writeln!(out, "    </signal>");
    }

    for property in interface.properties() {
        let name = property.name().to_string();
        let access = match (property.access().read(), property.access().write()) {
            (true, true) => "readwrite",
            (false, true) => "write",
            _ => "read",
        };
        let tp_type = references
            .get(&Site::Property(name.clone()))
            .map(|r| format!(r#" tp:type="{r}""#))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            r#"    <property name="{name}" type="{}" access="{access}"{tp_type}>"#,
            property.ty().inner()
        );
        write_docstring(
            out,
            member_docs(&name).and_then(|m| m.doc.as_deref()),
            "      ",
        );
        let _ = writeln!(out, "    </property>");
    }

    let _ = writeln!(out, "  </interface>");
}

fn write_arg(
    out: &mut String,
    arg: &zbus_xml::Arg,
    member: &str,
    direction: Option<&str>,
    references: &BTreeMap<Site, String>,
    member_docs: Option<&crate::annotate::MemberDocs>,
) {
    let name = arg.name().unwrap_or_default();
    let direction = direction
        .map(|d| format!(r#" direction="{d}""#))
        .unwrap_or_default();
    let tp_type = references
        .get(&Site::Arg(member.to_owned(), name.to_owned()))
        .map(|r| format!(r#" tp:type="{r}""#))
        .unwrap_or_default();
    let doc = member_docs
        .and_then(|m| m.args.get(name))
        .and_then(|a| a.doc.as_deref());

    let _ = writeln!(
        out,
        r#"      <arg name="{name}" type="{}"{direction}{tp_type}>"#,
        arg.ty().inner()
    );
    write_docstring(out, doc, "        ");
    let _ = writeln!(out, "      </arg>");
}

/// Write a `<tp:docstring>`, escaped so that the XML stays well-formed.
///
/// `zbus_xml` hands the element's contents to `zbus_xmlgen` verbatim, so the escapes survive
/// into the generated source and are undone there by [`crate::codegen::unescape_doc_comments`].
fn write_docstring(out: &mut String, doc: Option<&str>, indent: &str) {
    let Some(doc) = doc.filter(|d| !d.trim().is_empty()) else {
        return;
    };
    let _ = write!(out, "{indent}<tp:docstring>");
    let _ = write!(out, "{}", escape(doc));
    let _ = writeln!(out, "</tp:docstring>");
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Emit the Rust module holding NetworkManager's enumerations and flag sets.
pub fn types_module(types: &[Resolved<'_>], nm_version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! NetworkManager's enumerations and flag sets.\n\
         //!\n\
         //! Generated by `just generate` from the D-Bus interface headers of\n\
         //! NetworkManager {nm_version}. Do not edit.\n\
         \n\
         #![allow(rustdoc::invalid_html_tags)]\n"
    );
    let _ = writeln!(out, "use crate::repr::{{nm_enum, nm_flags}};\n");

    for resolved in types {
        let def = resolved.def;
        let variants = def.rust_variants();
        let macro_name = if def.is_flags() {
            "nm_flags"
        } else {
            "nm_enum"
        };

        let _ = writeln!(out, "{macro_name}! {{");
        if write_doc_lines(&mut out, def.doc.as_deref(), "    ") {
            let _ = writeln!(out, "    ///");
        }
        let _ = writeln!(
            out,
            "    /// `{}` in NetworkManager, sent over D-Bus as `{}`.",
            def.c_name, resolved.signature
        );
        // Marked in the introspection data rather than applied as `#[deprecated]`: the wire
        // still carries these, so a binding that warned on reading one would be unusable.
        if def.deprecated {
            let _ = writeln!(out, "    ///");
            let _ = writeln!(out, "    /// Deprecated by NetworkManager.");
        }
        let _ = writeln!(out, "    pub enum {}: {} {{", def.rust_name, resolved.repr);
        for (variant, name) in variants {
            if write_doc_lines(&mut out, variant.doc.as_deref(), "        ") {
                let _ = writeln!(out, "        ///");
            }
            let _ = writeln!(out, "        /// `{}`", variant.c_name);
            let _ = writeln!(out, "        {name} = {},", variant.value);
        }
        let _ = writeln!(out, "    }}\n}}\n");
    }

    out
}

/// Write `doc` as Rust doc comments, reporting whether anything was written.
fn write_doc_lines(out: &mut String, doc: Option<&str>, indent: &str) -> bool {
    let Some(doc) = doc.filter(|d| !d.trim().is_empty()) else {
        return false;
    };
    for line in rustdoc_text(doc).lines() {
        if line.is_empty() {
            let _ = writeln!(out, "{indent}///");
        } else {
            let _ = writeln!(out, "{indent}/// {line}");
        }
    }
    true
}

/// `IP_CONFIG` -> `IpConfig`, keeping words that carry digits (`802`, `P2P`) as written.
pub fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for word in name.split('_').filter(|w| !w.is_empty()) {
        if word.contains(|c: char| c.is_ascii_digit()) {
            out.push_str(word);
            continue;
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
    }
    out
}

/// The Rust integer type a D-Bus signature decodes to.
pub fn repr_for(signature: &str) -> Option<&'static str> {
    Some(match signature {
        "y" => "u8",
        "n" => "i16",
        "q" => "u16",
        "i" => "i32",
        "u" => "u32",
        "x" => "i64",
        "t" => "u64",
        _ => return None,
    })
}

/// Emit the Rust module of connection-profile settings.
///
/// Every property is an `Option`, because NetworkManager normalises a profile before validating
/// it: absent means "decide for me", present means "use exactly this", and the two are not the
/// same value. Properties and settings this binding does not know are kept in `other` maps so a
/// read-modify-write against a newer daemon does not drop them.
pub fn settings_module(
    settings: &[crate::settings::Setting],
    types: &BTreeMap<String, Resolved<'_>>,
    nm_version: &str,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Connection profiles, and the settings they are made of.\n\
         //!\n\
         //! Generated by `just generate` from the connection-profile property table of\n\
         //! NetworkManager {nm_version}. Do not edit.\n\
         \n\
         #![allow(rustdoc::invalid_html_tags, rustdoc::bare_urls)]\n\
         \n\
         use std::collections::HashMap;\n\
         \n\
         use zbus::zvariant::OwnedValue;\n\
         \n\
         use super::types::*;\n\
         use crate::settings::{{self, ProfileMap, Setting, SettingMap, SettingsError}};\n\
         \n\
         /// An address as the deprecated `ipv6.addresses` carries it: the address bytes, the\n\
         /// prefix length, and the gateway bytes. `address-data` is the form to prefer.\n\
         pub type LegacyIp6Address = (Vec<u8>, u32, Vec<u8>);\n\
         \n\
         /// A route as the deprecated `ipv6.routes` carries it: the destination bytes, the\n\
         /// prefix length, the next hop, and the metric. `route-data` is the form to prefer.\n\
         pub type LegacyIp6Route = (Vec<u8>, u32, Vec<u8>, u32);\n"
    );

    for setting in settings {
        write_setting_struct(&mut out, setting, types);
    }
    write_profile_struct(&mut out, settings);

    crate::codegen::unescape_doc_comments(&out)
}

fn write_setting_struct(
    out: &mut String,
    setting: &crate::settings::Setting,
    types: &BTreeMap<String, Resolved<'_>>,
) {
    let fields: Vec<_> = setting
        .properties
        .iter()
        .map(|property| (property, rust_type(&property.ty, types)))
        .collect();

    if let Some(doc) = &setting.doc {
        let _ = writeln!(out, "/// {doc}");
        let _ = writeln!(out, "///");
    }
    let _ = writeln!(
        out,
        "/// The `{}` setting of a connection profile.",
        setting.name
    );
    let _ = writeln!(out, "#[derive(Debug, Default, Clone, PartialEq)]");
    let _ = writeln!(out, "pub struct {} {{", setting.struct_name);

    for (property, ty) in &fields {
        if write_doc_lines(out, property.doc.as_deref(), "    ") {
            let _ = writeln!(out, "    ///");
        }
        let _ = writeln!(out, "    /// `{}`", property.name);
        if let Some(default) = &property.default {
            let _ = writeln!(out, "    ///");
            let _ = writeln!(out, "    /// Unset, NetworkManager uses `{default}`.");
        }
        let _ = writeln!(out, "    pub {}: Option<{ty}>,", property.field_name);
    }
    let _ = writeln!(
        out,
        "    /// Properties of this setting that this binding has no field for, kept so that\n    \
         /// reading a profile and writing it back does not drop them.\n    \
         pub other: SettingMap,\n}}\n"
    );

    // The trait implementation: one line per property in each direction.
    let _ = writeln!(out, "impl Setting for {} {{", setting.struct_name);
    let _ = writeln!(out, "    const NAME: &'static str = \"{}\";", setting.name);
    let _ = writeln!(out, "    const PROPERTIES: &'static [&'static str] = &[");
    for (property, _) in &fields {
        let _ = writeln!(out, "        \"{}\",", property.name);
    }
    let _ = writeln!(out, "    ];\n");

    let _ = writeln!(
        out,
        "    fn from_map(map: &SettingMap) -> Result<Self, SettingsError> {{\n        Ok(Self {{"
    );
    for (property, _) in &fields {
        let _ = writeln!(
            out,
            "            {}: settings::get(map, Self::NAME, \"{}\")?,",
            property.field_name, property.name
        );
    }
    let _ = writeln!(
        out,
        "            other: settings::unknown(map, Self::NAME, Self::PROPERTIES)?,\n        \
         }})\n    }}\n"
    );

    let _ = writeln!(
        out,
        "    fn to_map(&self) -> Result<SettingMap, SettingsError> {{\n        \
         let mut map = SettingMap::new();"
    );
    for (property, _) in &fields {
        let clone = if property.ty.is_copy() {
            ""
        } else {
            ".clone()"
        };
        let _ = writeln!(
            out,
            "        settings::set(&mut map, Self::NAME, \"{}\", self.{}{clone})?;",
            property.name, property.field_name
        );
    }
    let _ = writeln!(
        out,
        "        settings::merge_unknown(&mut map, Self::NAME, &self.other)?;\n        \
         Ok(map)\n    }}\n}}\n"
    );
}

fn write_profile_struct(out: &mut String, settings: &[crate::settings::Setting]) {
    let _ = writeln!(
        out,
        "/// A NetworkManager connection profile.\n\
         ///\n\
         /// Every setting is optional, and so is every property within one: NetworkManager\n\
         /// normalises a profile before validating it, so leaving something out asks it to\n\
         /// decide. Only [`connection.id`](ConnectionSetting::id) and one base-type setting —\n\
         /// [`wired`](Self::wired), [`wireless`](Self::wireless), and so on — are required, and\n\
         /// [`ConnectionProfile::new`] supplies the first.\n\
         #[derive(Debug, Default, Clone, PartialEq)]\n\
         pub struct ConnectionProfile {{"
    );
    for setting in settings {
        let _ = writeln!(out, "    /// The `{}` setting.", setting.name);
        let _ = writeln!(
            out,
            "    pub {}: Option<{}>,",
            setting.field_name, setting.struct_name
        );
    }
    let _ = writeln!(
        out,
        "    /// Settings this binding has no struct for, kept so that reading a profile and\n    \
         /// writing it back does not drop them.\n    \
         pub other: ProfileMap,\n}}\n"
    );

    let _ = writeln!(
        out,
        "impl ConnectionProfile {{\n    \
         /// A profile with the identifier NetworkManager requires.\n    \
         ///\n    \
         /// A base-type setting still has to be filled in for the profile to be accepted —\n    \
         /// which one determines what kind of connection it is:\n    \
         ///\n    \
         /// ```no_run\n    \
         /// # use netman::{{ConnectionProfile, WirelessSetting}};\n    \
         /// let mut profile = ConnectionProfile::new(\"home wifi\");\n    \
         /// profile.wireless = Some(WirelessSetting {{\n    \
         ///     ssid: Some(b\"my-network\".to_vec()),\n    \
         ///     ..Default::default()\n    \
         /// }});\n    \
         /// ```\n    \
         pub fn new(id: impl Into<String>) -> Self {{\n        \
         Self {{\n            \
         connection: Some(ConnectionSetting {{\n                \
         id: Some(id.into()),\n                \
         ..Default::default()\n            \
         }}),\n            \
         ..Default::default()\n        \
         }}\n    }}\n"
    );

    let known: Vec<&str> = settings.iter().map(|s| s.name.as_str()).collect();
    let _ = writeln!(
        out,
        "    /// The setting names this binding has a struct for.\n    \
         pub const SETTINGS: &'static [&'static str] = &["
    );
    for name in &known {
        let _ = writeln!(out, "        \"{name}\",");
    }
    let _ = writeln!(out, "    ];\n");

    let _ = writeln!(
        out,
        "    /// Read a profile as NetworkManager sent it.\n    \
         pub fn from_map(profile: &ProfileMap) -> Result<Self, SettingsError> {{\n        \
         Ok(Self {{"
    );
    for setting in settings {
        let _ = writeln!(
            out,
            "            {}: settings::setting_from::<{}>(profile)?,",
            setting.field_name, setting.struct_name
        );
    }
    let _ = writeln!(
        out,
        "            other: profile\n                \
         .iter()\n                \
         .filter(|(name, _)| !Self::SETTINGS.contains(&name.as_str()))\n                \
         .map(|(name, setting)| {{\n                    \
         Ok((name.clone(), settings::unknown(setting, \"\", &[])?))\n                \
         }})\n                \
         .collect::<Result<_, SettingsError>>()?,\n        \
         }})\n    }}\n"
    );

    let _ = writeln!(
        out,
        "    /// Render the profile in the form NetworkManager expects.\n    \
         pub fn to_map(&self) -> Result<ProfileMap, SettingsError> {{\n        \
         let mut profile = ProfileMap::new();"
    );
    for setting in settings {
        let _ = writeln!(
            out,
            "        settings::setting_into(&mut profile, self.{}.as_ref())?;",
            setting.field_name
        );
    }
    let _ = writeln!(
        out,
        "        for (name, setting) in &self.other {{\n            \
         profile.insert(name.clone(), settings::unknown(setting, \"\", &[])?);\n        \
         }}\n        \
         Ok(profile)\n    }}\n}}\n"
    );
}

/// The Rust type a connection-profile property becomes.
fn rust_type(ty: &crate::settings::PropertyType, types: &BTreeMap<String, Resolved<'_>>) -> String {
    use crate::settings::PropertyType as P;
    match ty {
        P::Bool => "bool".into(),
        P::Byte => "u8".into(),
        P::Int32 => "i32".into(),
        P::Uint32 => "u32".into(),
        P::Int64 => "i64".into(),
        P::Uint64 => "u64".into(),
        P::Str => "String".into(),
        P::StrArray => "Vec<String>".into(),
        P::ByteArray => "Vec<u8>".into(),
        P::ByteArrayArray => "Vec<Vec<u8>>".into(),
        P::Uint32Array => "Vec<u32>".into(),
        P::Uint32ArrayArray => "Vec<Vec<u32>>".into(),
        P::StringMap => "HashMap<String, String>".into(),
        P::VardictArray => "Vec<HashMap<String, OwnedValue>>".into(),
        P::LegacyIp6Addresses => "Vec<LegacyIp6Address>".into(),
        P::LegacyIp6Routes => "Vec<LegacyIp6Route>".into(),
        // A type the enumerations already cover; falling back to its width keeps the field
        // usable when the table names something the introspection data does not define.
        P::Named { c_name } => types
            .get(c_name)
            .map(|resolved| resolved.def.rust_name.clone())
            .unwrap_or_else(|| "u32".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_keeps_numbered_words() {
        assert_eq!(pascal_case("IP_CONFIG"), "IpConfig");
        assert_eq!(pascal_case("KEY_MGMT_802_1X"), "KeyMgmt8021X");
        assert_eq!(pascal_case("WIFI_P2P"), "WifiP2P");
        assert_eq!(pascal_case("ACTIVATED"), "Activated");
    }
}
