//! Emitters: augmented introspection XML, and the Rust module of NetworkManager's types.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use zbus_xml::{ArgDirection, Interface, Node};

use anyhow::Context as _;

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
    objects: &crate::objects::Targets,
    vardicts: &[crate::overrides::Vardict],
    newtypes: &[(&str, &str, &str)],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(out, r#"<node name="/" xmlns:tp="{TP_NS}">"#);

    // Every type the file's members refer to is defined at node level, where `zbus_xmlgen`
    // picks it up for whichever interfaces reference it.
    for name in referenced_types(node, docs, types) {
        write_type_def(&mut out, &types[&name]);
    }
    // `zbus_xmlgen` only names a type it has a definition for. The body it generates from this
    // one is stripped like all the others; the real struct is in the settings module.
    if node_carries_profile(node) {
        let _ = writeln!(
            out,
            r#"  <tp:simple-type name="ConnectionProfile" type="{PROFILE_SIGNATURE}"/>"#
        );
    }
    // Likewise for the paths this file's members point at.
    let mut path_types: Vec<String> = node
        .interfaces()
        .iter()
        .filter_map(|i| objects.get(i.name().as_str()))
        .flat_map(|sites| sites.values().cloned())
        .collect();
    path_types.sort();
    path_types.dedup();
    for interface in &path_types {
        let _ = writeln!(
            out,
            r#"  <tp:simple-type name="{}" type="{}"/>"#,
            path_type_name(&crate::codegen::module_name(interface)),
            crate::objects::PATH
        );
    }

    // The dictionary shapes any of this file's properties carry.
    let mut shapes: Vec<&str> = Vec::new();
    for interface in node.interfaces() {
        for property in interface.properties() {
            let member = format!("{}.{}", interface.name(), property.name());
            for vardict in vardicts {
                if vardict.applies.iter().any(|a| a == &member)
                    && !shapes.contains(&vardict.name.as_str())
                {
                    shapes.push(&vardict.name);
                }
            }
        }
    }
    for shape in &shapes {
        let _ = writeln!(
            out,
            r#"  <tp:simple-type name="{shape}" type="{VARDICT_SIGNATURE}"/>"#
        );
    }

    // And the hand-written types any of this file's members are read through.
    let mut wrapped: Vec<(&str, &str)> = Vec::new();
    for interface in node.interfaces() {
        for property in interface.properties() {
            if let Some((name, signature)) =
                newtype_for(newtypes, interface.name().as_str(), &property.name())
                && !wrapped.iter().any(|(other, _)| *other == name)
            {
                wrapped.push((name, signature));
            }
        }
    }
    for (name, signature) in &wrapped {
        let _ = writeln!(
            out,
            r#"  <tp:simple-type name="{name}" type="{signature}"/>"#
        );
    }

    for interface in node.interfaces() {
        write_interface(
            &mut out, interface, docs, types, objects, vardicts, newtypes,
        );
    }

    let _ = writeln!(out, "</node>");
    out
}

/// Whether any member of `node` carries a connection profile.
fn node_carries_profile(node: &Node<'_>) -> bool {
    node.interfaces().iter().any(|interface| {
        interface
            .properties()
            .iter()
            .any(|p| p.ty().to_string() == PROFILE_SIGNATURE)
            || interface
                .methods()
                .iter()
                .flat_map(|m| m.args())
                .chain(interface.signals().iter().flat_map(|s| s.args()))
                .any(|a| a.ty().to_string() == PROFILE_SIGNATURE)
    })
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
    /// The name the member is generated under, e.g. `nm_rename_000`.
    ///
    /// The index is fixed width so that no sentinel is a prefix of another: replacing
    /// `nm_rename_1` before `nm_rename_12` would corrupt the second.
    pub sentinel: String,
    /// The type name generated from the sentinel, e.g. `NmRename000`.
    pub sentinel_type: String,
    /// What the generated function should end up called, e.g. `state_changed_signal`.
    pub fn_name: String,
    /// What the generated types should end up called, e.g. `StateChangedSignal`.
    pub type_name: String,
    /// How the member is described in the doc comment `zbus_xmlgen` writes.
    pub kind: &'static str,
}

/// A member that returns object paths, and the accessor generated beside it.
///
/// The raw member is renamed out of the way so that the plain name can return something
/// usable: `get_devices` hands back proxies, and `get_device_paths` the paths it built them
/// from.
#[derive(Debug, Clone)]
pub struct PathAccessor {
    /// The renamed raw member, e.g. `get_device_paths`.
    pub raw: String,
    /// The accessor to generate, e.g. `get_devices`.
    pub wrapper: String,
    /// The interface objects at those paths implement.
    pub target: String,
    /// Whether the member returns a list of paths rather than one.
    pub list: bool,
    /// Whether the member is a property, whose path may be the null path `/`.
    pub property: bool,
    /// The argument a signal carries, when the member is one.
    pub signal_arg: Option<String>,
    /// How the member is described, for the accessor's documentation.
    pub description: String,
}

/// The name a path-returning member is moved to: `get_devices` -> `get_device_paths`.
fn raw_path_name(name: &str, list: bool) -> String {
    if !list {
        return format!("{name}_path");
    }
    // `get_devices` reads better as `get_device_paths` than `get_devices_paths`.
    match name.strip_suffix('s') {
        Some(singular) => format!("{singular}_paths"),
        None => format!("{name}_paths"),
    }
}

/// The renames and path accessors for an interface.
pub fn plan(
    interface: &Interface<'_>,
    objects: &BTreeMap<Site, String>,
) -> (Vec<Rename>, Vec<PathAccessor>) {
    let mut renames = plan_collisions(interface);
    let mut accessors = Vec::new();

    // Read-only path properties, and methods whose single output is a path.
    let mut candidates: Vec<(Site, String, bool, String)> = Vec::new();
    let mut signals: Vec<String> = Vec::new();
    for property in interface.properties() {
        let name = property.name().to_string();
        let signature = property.ty().to_string();
        if property.access().read() && matches!(signature.as_str(), "o" | "ao") {
            candidates.push((
                Site::Property(name.clone()),
                name.clone(),
                signature == "ao",
                format!("the `{name}` property"),
            ));
        }
    }
    for signal in interface.signals() {
        // Only a signal whose whole payload is one path can become a stream of objects;
        // anything else would have to change shape, not just type.
        let [arg] = signal.args() else { continue };
        let Some(arg_name) = arg.name() else { continue };
        if arg.ty().inner().to_string() != "o" {
            continue;
        }
        let member = signal.name().to_string();
        candidates.push((
            Site::Arg(member.clone(), arg_name.to_owned()),
            member.clone(),
            false,
            format!("the `{member}` signal"),
        ));
        signals.push(member);
    }

    for method in interface.methods() {
        let outputs: Vec<_> = method
            .args()
            .iter()
            .filter(|a| a.direction() == Some(ArgDirection::Out))
            .collect();
        // A method returning a tuple keeps its raw form: unpicking which element is a path
        // would change the shape of the return, not just its type.
        let [output] = outputs.as_slice() else {
            continue;
        };
        let Some(arg_name) = output.name() else {
            continue;
        };
        let signature = output.ty().inner().to_string();
        if matches!(signature.as_str(), "o" | "ao") {
            let member = method.name().to_string();
            candidates.push((
                Site::Arg(member.clone(), arg_name.to_owned()),
                member.clone(),
                signature == "ao",
                format!("the `{member}` method"),
            ));
        }
    }

    for (site, member, list, description) in candidates {
        let Some(target) = objects.get(&site) else {
            continue;
        };
        // A member already renamed for a collision keeps that name; wrapping it as well would
        // need the two rules to agree, and none of them overlap in practice.
        if renames.iter().any(|r| r.member == member) {
            continue;
        }
        let wrapper = snake(&member);
        let raw = raw_path_name(&wrapper, list);
        let index = renames.len();
        renames.push(Rename {
            member: member.clone(),
            sentinel: format!("nm_rename_{index:03}"),
            sentinel_type: format!("NmRename{index:03}"),
            fn_name: raw.clone(),
            type_name: pascal_case(&raw),
            kind: match (&site, signals.contains(&member)) {
                (Site::Property(_), _) => "property",
                (_, true) => "signal",
                _ => "method",
            },
        });
        let signal_arg = signals.contains(&member).then(|| match &site {
            Site::Arg(_, arg) => arg.clone(),
            Site::Property(name) => name.clone(),
        });
        accessors.push(PathAccessor {
            raw,
            wrapper,
            target: target.clone(),
            list,
            property: matches!(site, Site::Property(_)),
            signal_arg,
            description,
        });
    }

    (renames, accessors)
}

fn plan_collisions(interface: &Interface<'_>) -> Vec<Rename> {
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
            sentinel: format!("nm_rename_{index:03}"),
            sentinel_type: format!("NmRename{index:03}"),
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
    objects: &crate::objects::Targets,
    vardicts: &[crate::overrides::Vardict],
    newtypes: &[(&str, &str, &str)],
) {
    let empty = BTreeMap::new();
    let object_sites = objects.get(interface.name().as_str()).unwrap_or(&empty);
    let references = interface_references(interface, docs, types);
    let iface_docs = docs.interfaces.get(interface.name().as_str());
    let member_docs = |name: &str| iface_docs.and_then(|i| i.members.get(name));

    let _ = writeln!(out, r#"  <interface name="{}">"#, interface.name());
    write_docstring(out, iface_docs.and_then(|i| i.doc.as_deref()), "    ");

    let renames = plan(interface, object_sites).0;
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
            write_arg(
                out,
                arg,
                &name,
                Some(direction),
                &references,
                object_sites,
                member,
            );
        }
        let _ = writeln!(out, "    </method>");
    }

    for signal in interface.signals() {
        let name = signal.name().to_string();
        let member = member_docs(&name);
        let _ = writeln!(out, r#"    <signal name="{}">"#, emitted_name(&name));
        write_docstring(out, member.and_then(|m| m.doc.as_deref()), "      ");
        for arg in signal.args() {
            write_arg(out, arg, &name, None, &references, object_sites, member);
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
        let site = Site::Property(name.clone());
        let tp_type = references
            .get(&site)
            .map(|r| format!(r#" tp:type="{r}""#))
            .or_else(|| {
                let (wrapper, _) = newtype_for(newtypes, interface.name().as_str(), &name)?;
                Some(format!(r#" tp:type="{wrapper}""#))
            })
            .or_else(|| object_attribute(object_sites, &site, &property.ty().to_string()))
            .or_else(|| {
                vardict_attribute(
                    vardicts,
                    interface.name().as_str(),
                    &name,
                    &property.ty().to_string(),
                )
            })
            .unwrap_or_else(|| profile_attribute(&property.ty().to_string()).to_owned());
        let _ = writeln!(
            out,
            r#"    <property name="{}" type="{}" access="{access}"{tp_type}>"#,
            emitted_name(&name),
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
    object_sites: &BTreeMap<Site, String>,
    member_docs: Option<&crate::annotate::MemberDocs>,
) {
    let name = arg.name().unwrap_or_default();
    let direction = direction
        .map(|d| format!(r#" direction="{d}""#))
        .unwrap_or_default();
    let site = Site::Arg(member.to_owned(), name.to_owned());
    let tp_type = references
        .get(&site)
        .map(|r| format!(r#" tp:type="{r}""#))
        .or_else(|| object_attribute(object_sites, &site, &arg.ty().inner().to_string()))
        .unwrap_or_else(|| profile_attribute(&arg.ty().inner().to_string()).to_owned());
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
pub fn types_module(
    types: &[Resolved<'_>],
    values: &[crate::values::ValueSet],
    nm_version: &str,
) -> String {
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
    let _ = writeln!(
        out,
        "use crate::repr::{{nm_enum, nm_flags, nm_str_enum}};\n"
    );

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

    // The properties that travel as a string from a fixed set. NetworkManager declares most of
    // these as constants and the rest only in its prose; see `crate::values`.
    for set in values {
        let _ = writeln!(out, "nm_str_enum! {{");
        if write_doc_lines(&mut out, set.doc.as_deref(), "    ") {
            let _ = writeln!(out, "    ///");
        }
        let _ = writeln!(
            out,
            "    /// The values `{}.{}` takes, sent over D-Bus as `s`.",
            set.setting, set.property
        );
        let _ = writeln!(out, "    pub enum {} {{", set.rust_name);
        for value in &set.values {
            if write_doc_lines(&mut out, value.doc.as_deref(), "        ") {
                let _ = writeln!(out, "        ///");
            }
            match &value.c_name {
                Some(c_name) => {
                    let _ = writeln!(out, "        /// `{c_name}`");
                }
                // Nothing upstream names it, so the string itself is the whole story.
                None => {
                    let _ = writeln!(out, "        /// `{}`", value.text);
                }
            }
            let _ = writeln!(out, "        {} = \"{}\",", value.rust_name, value.text);
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
        let mut chars = word.chars();
        let Some(first) = chars.next() else { continue };
        out.push(first.to_ascii_uppercase());
        // A word carrying digits keeps its spelling, so `802` and `P2P` survive; only the
        // first character is ever forced, or `ip4_config` would stay lower case.
        if word.contains(|c: char| c.is_ascii_digit()) {
            out.push_str(chars.as_str());
        } else {
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
    // The hand-written types some properties are read through, which unlike the generated ones
    // live at the crate root.
    let mut used: Vec<&str> = settings
        .iter()
        .flat_map(|setting| &setting.properties)
        .filter_map(|property| match &property.ty {
            crate::settings::PropertyType::Newtype { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    used.sort_unstable();
    used.dedup();
    let newtypes = match used.is_empty() {
        true => String::new(),
        false => format!("use crate::{{{}}};\n", used.join(", ")),
    };

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
         {newtypes}\
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

    let setters: Vec<(&str, &str, String)> = fields
        .iter()
        .map(|(property, ty)| {
            (
                property.field_name.as_str(),
                property.name.as_str(),
                setter_argument(&property.ty, ty),
            )
        })
        .collect();
    write_setters(out, &setting.struct_name, "property", &setters);

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

/// Emit the setters that fill a struct in without `Some` at every call site.
///
/// Each takes and returns the value, so they chain from a `default()`, and each sets the field
/// of the same name — a method and a field can share one, and anything else would be a second
/// spelling of a property to remember.
fn write_setters(
    out: &mut String,
    struct_name: &str,
    kind: &str,
    setters: &[(&str, &str, String)],
) {
    if setters.is_empty() {
        return;
    }
    let _ = writeln!(out, "impl {struct_name} {{");
    for (field, name, argument) in setters {
        // `field@` because the method and the field share a name, which rustdoc would
        // otherwise call an ambiguous link.
        // Only the ones that take a conversion do one; `Some(value.into())` on a value already
        // of the right type is a useless conversion, as clippy rightly says.
        let value = match argument.starts_with("impl Into<") {
            true => "value.into()",
            false => "value",
        };
        let _ = writeln!(
            out,
            "    /// Set [`{field}`](field@Self::{field}), the `{name}` {kind}.\n    \
             #[must_use]\n    \
             pub fn {field}(mut self, value: {argument}) -> Self {{\n        \
             self.{field} = Some({value});\n        \
             self\n    \
             }}\n"
        );
    }
    let _ = writeln!(out, "}}\n");
}

/// What a setter takes: the value itself, or anything that converts into it.
///
/// `impl Into` where a conversion is worth having — `ssid("home")`, `id("home wifi")` — and the
/// plain type everywhere else. Not on the numbers: an integer literal has no type of its own,
/// so `mtu(1500)` against `impl Into<u32>` would fail to infer one and have to be written
/// `1500u32`, which is a worse trade than the conversion is worth.
fn setter_argument(ty: &crate::settings::PropertyType, rust_type: &str) -> String {
    use crate::settings::PropertyType as P;
    match ty {
        P::Str | P::Newtype { .. } => format!("impl Into<{rust_type}>"),
        _ => rust_type.to_owned(),
    }
}

fn write_profile_struct(out: &mut String, settings: &[crate::settings::Setting]) {
    let _ = writeln!(
        out,
        "/// A NetworkManager connection profile.\n\
         ///\n\
         /// Every setting is optional, and so is every property within one: NetworkManager\n\
         /// normalises a profile before validating it, so leaving something out asks it to\n\
         /// decide. Only [`connection.id`](field@ConnectionSetting::id) and one base-type\n\
         /// setting — [`wired`](field@Self::wired), [`wireless`](field@Self::wireless), and so\n\
         /// on — are required, and [`ConnectionProfile::new`] supplies the first.\n\
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

    let setters: Vec<(&str, &str, String)> = settings
        .iter()
        .map(|setting| {
            (
                setting.field_name.as_str(),
                setting.name.as_str(),
                setting.struct_name.clone(),
            )
        })
        .collect();
    write_setters(out, "ConnectionProfile", "setting", &setters);

    let _ = writeln!(
        out,
        "impl ConnectionProfile {{\n    \
         /// A profile with the identifier NetworkManager requires.\n    \
         ///\n    \
         /// A base-type setting still has to be filled in for the profile to be accepted —\n    \
         /// which one determines what kind of connection it is:\n    \
         ///\n    \
         /// ```no_run\n    \
         /// # use nm_automata::{{ConnectionProfile, WirelessMode, WirelessSetting}};\n    \
         /// let profile = ConnectionProfile::new(\"home wifi\").wireless(\n    \
         ///     WirelessSetting::default()\n    \
         ///         .ssid(\"my-network\")\n    \
         ///         .mode(WirelessMode::Infrastructure),\n    \
         /// );\n    \
         /// ```\n    \
         ///\n    \
         /// Every property and every setting has a setter of its own name, which is the same\n    \
         /// field written without the `Some`; the structs are plain, so a literal with\n    \
         /// `..Default::default()` does just as well.\n    \
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

    // With these, `#[proxy]` can use a profile directly: every `a{sa{sv}}` argument and return
    // value becomes a `ConnectionProfile` instead of a map the caller has to convert.
    let _ = writeln!(
        out,
        "impl zbus::zvariant::Type for ConnectionProfile {{\n    \
         const SIGNATURE: &'static zbus::zvariant::Signature =\n        \
         <ProfileMap as zbus::zvariant::Type>::SIGNATURE;\n\
         }}\n\
         \n\
         impl serde::Serialize for ConnectionProfile {{\n    \
         fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {{\n        \
         // Through `settings::serialize_profile` because `to_map` builds a fresh map every\n        \
         // call and zvariant serialises twice; see there.\n        \
         settings::serialize_profile(&self.to_map().map_err(serde::ser::Error::custom)?, serializer)\n    \
         }}\n\
         }}\n\
         \n\
         impl<'de> serde::Deserialize<'de> for ConnectionProfile {{\n    \
         fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {{\n        \
         let map = <ProfileMap as serde::Deserialize>::deserialize(deserializer)?;\n        \
         Self::from_map(&map).map_err(serde::de::Error::custom)\n    \
         }}\n\
         }}\n"
    );
}

/// The signature a connection profile travels as.
///
/// Every argument and property with this signature is a profile — a whole one, or the subset
/// of it that carries secrets — so no annotation data is needed to recognise them.
pub const PROFILE_SIGNATURE: &str = "a{sa{sv}}";

/// The hand-written type a member is read through, with the signature it reads.
///
/// `zbus_xmlgen` only resolves a `tp:type` whose declared signature matches the member's, so a
/// wrong one would quietly leave the member as its structural type; that is what
/// [`crate::values::check_member_newtypes`] is for.
fn newtype_for<'a>(
    newtypes: &[(&'a str, &str, &'a str)],
    interface: &str,
    property: &str,
) -> Option<(&'a str, &'a str)> {
    newtypes
        .iter()
        .find(|(i, member, _)| *i == interface && *member == property)
        .and_then(|(_, _, ty)| {
            let (_, signature) = crate::values::newtype_wire(ty)?;
            Some((*ty, signature))
        })
}

/// The `tp:type` attribute for a property whose dictionary shape is written down.
fn vardict_attribute(
    vardicts: &[crate::overrides::Vardict],
    interface: &str,
    property: &str,
    signature: &str,
) -> Option<String> {
    if signature != "aa{sv}" {
        return None;
    }
    let member = format!("{interface}.{property}");
    vardicts
        .iter()
        .find(|vardict| vardict.applies.iter().any(|a| a == &member))
        .map(|vardict| format!(r#" tp:type="{}[]""#, vardict.name))
}

/// The `tp:type` attribute for a member whose object path has a known target.
fn object_attribute(
    sites: &BTreeMap<Site, String>,
    site: &Site,
    signature: &str,
) -> Option<String> {
    let interface = sites.get(site)?;
    let name = path_type_name(&crate::codegen::module_name(interface));
    match signature {
        crate::objects::PATH => Some(format!(r#" tp:type="{name}""#)),
        crate::objects::PATH_ARRAY => Some(format!(r#" tp:type="{name}[]""#)),
        _ => None,
    }
}

/// The `tp:type` attribute for a member whose signature says it carries a profile.
fn profile_attribute(signature: &str) -> &'static str {
    if signature == PROFILE_SIGNATURE {
        r#" tp:type="ConnectionProfile""#
    } else {
        ""
    }
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
        P::Value { name } | P::Newtype { name } => name.clone(),
        P::ValueArray { name } => format!("Vec<{name}>"),
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

/// Emit the module of typed object paths.
///
/// Each is a newtype over an object path that knows the interface at the other end, so it can
/// build the right proxy and only that one.
pub fn paths_module(
    targets: &crate::objects::Targets,
    modules: &[crate::codegen::Generated],
    nm_version: &str,
) -> String {
    // One for every interface, not only the ones something points at: each proxy names its
    // own path through `object_path`.
    let _ = targets;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Object paths that know what they point at.\n\
         //!\n\
         //! Generated by `just generate` from the introspection XML of NetworkManager\n\
         //! {nm_version}. Do not edit.\n\
         \n\
         use crate::path::nm_path;\n"
    );

    for module in modules {
        let interface = &module.interface;
        let path_name = path_type_name(&module.module);

        let _ = writeln!(out, "nm_path! {{");
        let _ = writeln!(
            out,
            "    /// The path of an object implementing `{interface}`."
        );
        let _ = writeln!(out, "    pub struct {path_name}: \"{interface}\";");
        let _ = writeln!(out, "}}\n");
        let _ = writeln!(
            out,
            "impl crate::path::NmPath for {path_name} {{\n    \
             type Proxy = crate::proxies::{module}::{trait_name}Proxy<'static>;\n    \
             const INTERFACE: &'static str = \"{interface}\";\n\
             \n    \
             async fn connect(&self, connection: &zbus::Connection) -> zbus::Result<Self::Proxy> {{\n        \
             crate::ObjectPathExt::proxy(self.as_object_path(), connection).await\n    \
             }}\n\
             }}\n",
            module = module.module,
            trait_name = module.trait_name,
        );
    }

    out
}

/// The parameters of a generated member, as written and as forwarded.
///
/// Returns the declaration to repeat after `&self` — `, iface: &str` — and the names to pass
/// on — `iface`. `None` if the member is not there, which leaves it unwrapped rather than
/// generating a call that cannot compile.
fn parameters(code: &str, member: &str) -> Option<(String, String)> {
    let start = code.find(&format!(" fn {member}("))? + format!(" fn {member}(").len();
    let mut depth = 1usize;
    let mut end = start;
    for (offset, ch) in code[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + offset;
                    break;
                }
            }
            _ => {}
        }
    }
    let inside = code.get(start..end)?.trim();
    let rest = inside.strip_prefix("&self")?.trim_start_matches(',').trim();
    if rest.is_empty() {
        return Some((String::new(), String::new()));
    }

    // Split on the commas between parameters, ignoring those inside a type.
    let mut names = Vec::new();
    let (mut depth, mut last) = (0i32, 0usize);
    for (offset, ch) in rest.char_indices() {
        match ch {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                names.push(rest[last..offset].trim());
                last = offset + 1;
            }
            _ => {}
        }
    }
    names.push(rest[last..].trim());

    let forwarded: Vec<&str> = names
        .iter()
        .filter_map(|param| param.split(':').next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();

    Some((format!(", {rest}"), forwarded.join(", ")))
}

/// The path newtype for a module: `device_wireless` -> `DeviceWirelessPath`.
pub fn path_type_name(module: &str) -> String {
    format!("{}Path", pascal_case(module))
}

/// Emit the enumeration that narrows a device to the interface its type implies.
///
/// This is what replaces casting a `DeviceProxy` to a `WirelessProxy` and hoping. The type is
/// read once, and each variant carries a proxy that could only have been built after the
/// matching type was seen — so a `WirelessProxy` for a wired device cannot be constructed.
pub fn devices_module(
    device_type: &TypeDef,
    interfaces: &BTreeMap<String, String>,
    modules: &[crate::codegen::Generated],
    nm_version: &str,
) -> String {
    // Each device type that has an interface, with the proxy and variant it becomes.
    let arms: Vec<(String, &crate::codegen::Generated, String)> = device_type
        .rust_variants()
        .into_iter()
        .filter_map(|(variant, variant_name)| {
            let interface = interfaces.get(&variant.nick)?;
            let module = modules.iter().find(|m| &m.interface == interface)?;
            Some((
                variant_name,
                module,
                crate::codegen::root_alias(modules, module, ""),
            ))
        })
        .collect();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Narrowing a device to what it actually is.\n\
         //!\n\
         //! Generated by `just generate` from the introspection data of NetworkManager\n\
         //! {nm_version}. Do not edit.\n\
         \n\
         use crate::generated::proxies;\n\
         use crate::generated::types::DeviceType;\n"
    );

    let _ = writeln!(
        out,
        "/// A device, narrowed to the interface its [`DeviceType`] implies.\n\
         ///\n\
         /// Produced by [`DeviceProxy::specialise`](crate::DeviceProxy::specialise), which\n\
         /// reads the type once. A variant can only be built after its type has been seen, so\n\
         /// unlike a cast there is no way to hold a wireless proxy for a wired device.\n\
         #[derive(Debug, Clone)]\n\
         #[non_exhaustive]\n\
         pub enum AnyDevice {{"
    );
    for (variant_name, module, alias) in &arms {
        let _ = writeln!(
            out,
            "    /// [`DeviceType::{variant_name}`], which implements `{}`.",
            module.interface
        );
        let _ = writeln!(
            out,
            "    {alias}(proxies::{}::{}Proxy<'static>),",
            module.module, module.trait_name
        );
    }
    let _ = writeln!(
        out,
        "    /// A device whose type has no interface of its own — a loopback or a generic\n    \
         /// device — or one added to NetworkManager since this binding was generated.\n    \
         Other(DeviceType),\n\
         }}\n"
    );

    let _ = writeln!(
        out,
        "impl AnyDevice {{\n    \
         /// The type NetworkManager reported, whichever variant this is.\n    \
         pub fn device_type(&self) -> DeviceType {{\n        \
         match self {{"
    );
    for (variant_name, _, alias) in &arms {
        let _ = writeln!(
            out,
            "            Self::{alias}(_) => DeviceType::{variant_name},"
        );
    }
    let _ = writeln!(
        out,
        "            Self::Other(ty) => *ty,\n        }}\n    }}\n}}\n"
    );

    let _ = writeln!(
        out,
        "impl proxies::device::DeviceProxy<'_> {{\n    \
         /// Narrow this device to the interface its type implies.\n    \
         ///\n    \
         /// ```no_run\n    \
         /// # use nm_automata::{{AnyDevice, DeviceProxy}};\n    \
         /// # async fn example(device: DeviceProxy<'_>) -> zbus::Result<()> {{\n    \
         /// match device.specialise().await? {{\n    \
         ///     AnyDevice::Wireless(wifi) => {{\n    \
         ///         println!(\"{{:?}}\", wifi.get_access_points().await?);\n    \
         ///     }}\n    \
         ///     AnyDevice::Wired(wired) => {{\n    \
         ///         println!(\"{{}}\", wired.speed().await?);\n    \
         ///     }}\n    \
         ///     other => println!(\"{{:?}}\", other.device_type()),\n    \
         /// }}\n    \
         /// # Ok(())\n    \
         /// # }}\n    \
         /// ```\n    \
         pub async fn specialise(&self) -> zbus::Result<AnyDevice> {{\n        \
         use crate::ProxyExt as _;\n        \
         let device_type = self.device_type().await?;\n        \
         Ok(match device_type {{"
    );
    for (variant_name, _, alias) in &arms {
        let _ = writeln!(
            out,
            "            DeviceType::{variant_name} => AnyDevice::{alias}(self.cast().await?),"
        );
    }
    let _ = writeln!(
        out,
        "            other => AnyDevice::Other(other),\n        \
         }})\n    }}\n}}\n"
    );

    out
}

/// Emit the accessors that turn a member's object paths into the proxies they point at.
///
/// These are what make `get_devices` hand back devices rather than paths. They live on the
/// proxy as inherent methods, using the connection the proxy already holds, so nothing has to
/// be passed in.
pub fn path_accessors(
    code: &str,
    accessors: &[PathAccessor],
    trait_name: &str,
    module: &str,
    interfaces: &[String],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\nimpl {trait_name}Proxy<'_> {{");

    // Where this proxy is, in the form the members that take a path expect. `path` is not
    // available as a name: `Device.Path` is a real property, the one udev calls `ID_PATH`.
    let _ = writeln!(
        out,
        "    /// The path of the object this proxy is for.\n    \
         ///\n    \
         /// Typed, so it can be handed to a member that takes one — activating a connection\n    \
         /// wants the device's path, and this is where to get it.\n    \
         pub fn object_path(&self) -> crate::paths::{path} {{\n        \
         crate::paths::{path}::from(zbus::zvariant::OwnedObjectPath::from(\n            \
         self.inner().path().to_owned(),\n        \
         ))\n    \
         }}\n",
        path = path_type_name(module)
    );
    for accessor in accessors {
        if !interfaces.iter().any(|i| i == &accessor.target) {
            continue;
        }
        let module = crate::codegen::module_name(&accessor.target);
        let target_trait = accessor
            .target
            .rsplit('.')
            .next()
            .unwrap_or(&accessor.target);
        let proxy = format!("crate::proxies::{module}::{target_trait}Proxy<'static>");

        // A signal becomes a stream of objects rather than an accessor.
        if let Some(arg) = &accessor.signal_arg {
            let _ = writeln!(
                out,
                "    /// The objects {description} carries, as they arrive.\n    \
                 ///\n    \
                 /// Each is built on this proxy's own connection.\n    \
                 /// [`receive_{raw}`](Self::receive_{raw}) gives the signal itself, carrying\n    \
                 /// the path.\n    \
                 pub async fn receive_{wrapper}(\n        \
                 &self,\n    \
                 ) -> zbus::Result<impl futures_util::Stream<Item = zbus::Result<{proxy}>> + use<>> {{\n        \
                 let connection = self.inner().connection().clone();\n        \
                 let signals = self.receive_{raw}().await?;\n        \
                 Ok(futures_util::StreamExt::then(signals, move |signal| {{\n            \
                 let connection = connection.clone();\n            \
                 // Boxed so the stream stays `Unpin` and can be polled with `next()`\n            \
                 // without pinning it first.\n            \
                 Box::pin(async move {{\n                \
                 crate::NmPath::connect(signal.args()?.{arg}(), &connection).await\n            \
                 }})\n        \
                 }}))\n    \
                 }}\n",
                description = accessor.description,
                raw = accessor.raw,
                wrapper = accessor.wrapper,
            );
            continue;
        }

        // The raw member may take arguments; rather than work out their Rust types a second
        // time, its parameter list is taken from the signature just generated for it.
        let Some((declaration, forwarded)) = parameters(code, &accessor.raw) else {
            continue;
        };

        let _ = writeln!(
            out,
            "    /// The objects {} points at.\n    \
             ///\n    \
             /// Built on this proxy's own connection. [`{raw}`](Self::{raw}) gives the paths\n    \
             /// themselves.",
            accessor.description,
            raw = accessor.raw
        );
        // A property with nothing to point at points at `/`, where there is no object of any
        // kind — so the accessor says so, rather than handing back a proxy whose every call
        // would fail. A method returns an error instead of `/`, and a list leaves the object
        // out rather than listing it, so neither of those needs it.
        if accessor.property && !accessor.list {
            let _ = writeln!(
                out,
                "    ///\n    \
                 /// [`None`] if the property is [`/`](crate::NULL_PATH), which is how\n    \
                 /// NetworkManager says there is no such object.\n    \
                 pub async fn {}(&self{declaration}) -> zbus::Result<Option<{proxy}>> {{\n        \
                 let Some(path) = self.{}({forwarded}).await?.non_null() else {{\n            \
                 return Ok(None);\n        \
                 }};\n        \
                 crate::NmPath::connect(&path, self.inner().connection())\n            \
                 .await\n            \
                 .map(Some)\n    \
                 }}\n",
                accessor.wrapper, accessor.raw
            );
        } else if accessor.list {
            let _ = writeln!(
                out,
                "    pub async fn {}(&self{declaration}) -> zbus::Result<Vec<{proxy}>> {{\n        \
                 let paths = self.{}({forwarded}).await?;\n        \
                 crate::NmPathSliceExt::connect_all(&paths, self.inner().connection()).await\n    \
                 }}\n",
                accessor.wrapper, accessor.raw
            );
        } else {
            let _ = writeln!(
                out,
                "    pub async fn {}(&self{declaration}) -> zbus::Result<{proxy}> {{\n        \
                 let path = self.{}({forwarded}).await?;\n        \
                 crate::NmPath::connect(&path, self.inner().connection()).await\n    \
                 }}\n",
                accessor.wrapper, accessor.raw
            );
        }
    }
    let _ = writeln!(out, "}}\n");
    out
}

/// Emit the module of dictionary shapes.
///
/// These are the `aa{sv}` properties whose keys NetworkManager describes only in prose. Every
/// one is read-only, so a key typed wrongly is a decode error and not a corrupted profile —
/// which is what makes hand-written shapes acceptable here.
pub fn vardict_module(
    vardicts: &[crate::overrides::Vardict],
    types: &BTreeMap<String, Resolved<'_>>,
    nm_version: &str,
) -> Result<String, anyhow::Error> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! The dictionaries NetworkManager reports configuration in.\n\
         //!\n\
         //! Generated by `just generate` from `annotations.toml`, against NetworkManager\n\
         //! {nm_version}. Do not edit.\n\
         \n\
         #[allow(unused_imports)]\n\
         use std::collections::HashMap;\n\
         #[allow(unused_imports)]\n\
         use zbus::zvariant::OwnedValue;\n\
         #[allow(unused_imports)]\n\
         use super::types::*;\n\
         use crate::settings::{{self, SettingMap, SettingsError}};\n"
    );

    for vardict in vardicts {
        let fields: Vec<(&crate::overrides::VardictKey, String, bool)> = vardict
            .keys
            .iter()
            .map(|key| {
                let ty = crate::settings::PropertyType::parse(&key.ty)
                    .with_context(|| format!("the type of `{}.{}`", vardict.name, key.name))?;
                Ok((key, rust_type(&ty, types), ty.is_copy()))
            })
            .collect::<Result<_, anyhow::Error>>()?;

        let _ = writeln!(out, "/// {}", vardict.doc);
        let _ = writeln!(
            out,
            "///\n/// Keys NetworkManager sends that are not listed here are kept in\n\
             /// [`other`](Self::other), since it documents most of these as carrying\n\
             /// additional attributes."
        );
        let _ = writeln!(out, "#[derive(Debug, Default, Clone, PartialEq)]");
        let _ = writeln!(out, "pub struct {} {{", vardict.name);
        for (key, ty, _) in &fields {
            let _ = writeln!(out, "    /// {}", key.doc);
            let _ = writeln!(out, "    ///");
            let _ = writeln!(out, "    /// `{}`", key.name);
            let _ = writeln!(out, "    pub {}: Option<{ty}>,", field_ident(&key.name));
        }
        let _ = writeln!(
            out,
            "    /// Keys with no field of their own.\n    pub other: SettingMap,\n}}\n"
        );

        let _ = writeln!(out, "impl {} {{", vardict.name);
        let _ = writeln!(
            out,
            "    const KEYS: &'static [&'static str] = &[{}];\n",
            vardict
                .keys
                .iter()
                .map(|key| format!("\"{}\"", key.name))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(
            out,
            "    /// Read one of these as NetworkManager sent it.\n    \
             pub fn from_map(map: &SettingMap) -> Result<Self, SettingsError> {{\n        \
             Ok(Self {{"
        );
        for (key, _, _) in &fields {
            let _ = writeln!(
                out,
                "            {}: settings::get(map, \"{}\", \"{}\")?,",
                field_ident(&key.name),
                vardict.name,
                key.name
            );
        }
        let _ = writeln!(
            out,
            "            other: settings::unknown(map, \"{}\", Self::KEYS)?,\n        \
             }})\n    }}\n",
            vardict.name
        );
        let _ = writeln!(
            out,
            "    /// Render it in the form NetworkManager sends.\n    \
             pub fn to_map(&self) -> Result<SettingMap, SettingsError> {{\n        \
             let mut map = SettingMap::new();"
        );
        for (key, _, is_copy) in &fields {
            let clone = if *is_copy { "" } else { ".clone()" };
            let _ = writeln!(
                out,
                "        settings::set(&mut map, \"{}\", \"{}\", self.{}{clone})?;",
                vardict.name,
                key.name,
                field_ident(&key.name)
            );
        }
        let _ = writeln!(
            out,
            "        settings::merge_unknown(&mut map, \"{}\", &self.other)?;\n        \
             Ok(map)\n    }}\n}}\n",
            vardict.name
        );

        let _ = writeln!(
            out,
            "impl zbus::zvariant::Type for {name} {{\n    \
             const SIGNATURE: &'static zbus::zvariant::Signature =\n        \
             <SettingMap as zbus::zvariant::Type>::SIGNATURE;\n\
             }}\n\
             \n\
             impl serde::Serialize for {name} {{\n    \
             fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {{\n        \
             // Through `settings::serialize_setting` because `to_map` builds a fresh map every\n        \
             // call and zvariant serialises twice; see there.\n        \
             settings::serialize_setting(&self.to_map().map_err(serde::ser::Error::custom)?, serializer)\n    \
             }}\n\
             }}\n\
             \n\
             impl<'de> serde::Deserialize<'de> for {name} {{\n    \
             fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {{\n        \
             let map = <SettingMap as serde::Deserialize>::deserialize(deserializer)?;\n        \
             Self::from_map(&map).map_err(serde::de::Error::custom)\n    \
             }}\n\
             }}\n\
             \n\
             impl TryFrom<zbus::zvariant::OwnedValue> for {name} {{\n    \
             type Error = zbus::zvariant::Error;\n\
             \n    \
             fn try_from(value: zbus::zvariant::OwnedValue) -> Result<Self, Self::Error> {{\n        \
             let map = SettingMap::try_from(value)?;\n        \
             Self::from_map(&map).map_err(|e| zbus::zvariant::Error::Message(e.to_string()))\n    \
             }}\n\
             }}\n\
             \n\
             impl<'a> TryFrom<zbus::zvariant::Value<'a>> for {name} {{\n    \
             type Error = zbus::zvariant::Error;\n\
             \n    \
             fn try_from(value: zbus::zvariant::Value<'a>) -> Result<Self, Self::Error> {{\n        \
             Self::try_from(zbus::zvariant::OwnedValue::try_from(value)?)\n    \
             }}\n\
             }}\n",
            name = vardict.name
        );
    }

    Ok(crate::codegen::unescape_doc_comments(&out))
}

/// `next-hop` -> `next_hop`.
fn field_ident(name: &str) -> String {
    name.replace('-', "_").to_ascii_lowercase()
}

/// The signature one of these dictionaries travels as.
pub const VARDICT_SIGNATURE: &str = "a{sv}";

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
