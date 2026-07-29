//! Emitters: augmented introspection XML, and the Rust module of NetworkManager's types.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use zbus_xml::{ArgDirection, Interface, Node};

use crate::annotate::{Docs, rustdoc_text};
use crate::header::TypeDef;

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
    for variant in dedup_variants(def) {
        let _ = writeln!(
            out,
            r#"    <tp:enumvalue suffix="{}" value="{}"/>"#,
            variant.suffix, variant.value
        );
    }
    let _ = writeln!(out, "  </tp:enum>");
}

/// The variants that can become distinct Rust items.
///
/// NetworkManager marks the end of some flag sets with an alias (`…_CAPABILITY_LAST`); a
/// repeated value would make the whole definition unusable to `zbus_xmlgen`, so aliases are
/// dropped in favour of the name they duplicate.
pub fn dedup_variants(def: &TypeDef) -> Vec<&crate::header::Variant> {
    let mut seen_values = Vec::new();
    let mut seen_names = Vec::new();
    let mut out = Vec::new();
    for variant in &def.variants {
        if seen_values.contains(&variant.value) || seen_names.contains(&variant.suffix) {
            continue;
        }
        seen_values.push(variant.value);
        seen_names.push(variant.suffix.clone());
        out.push(variant);
    }
    out
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
        let variants = dedup_variants(def);
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
        let _ = writeln!(out, "    pub enum {}: {} {{", def.rust_name, resolved.repr);
        for variant in variants {
            write_doc_lines(&mut out, variant.doc.as_deref(), "        ");
            let _ = writeln!(out, "        /// `{}`", variant.c_name);
            let name = if def.is_flags() {
                variant.suffix.clone()
            } else {
                pascal_case(&variant.suffix)
            };
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
