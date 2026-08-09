//! Recovery of the documentation and type annotations NetworkManager hides in XML comments.
//!
//! The introspection XML is plain D-Bus: an enumeration-valued property is just `type="u"`.
//! The real types are named in the gtk-doc comment above each member, either as a DocBook
//! link or as a bare `NM…` word:
//!
//! ```xml
//! <!--
//!     State:
//!
//!     The current state of the device.
//!
//!     Returns: <link linkend="NMDeviceState">NMDeviceState</link>
//! -->
//! <property name="State" type="u" access="read"/>
//! ```
//!
//! This module lifts those comments back out, so that [`crate::render::resolved_names`] can
//! tell [`crate::proxy`] what each member really carries.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};

#[derive(Debug, Default)]
pub struct Docs {
    pub interfaces: BTreeMap<String, InterfaceDocs>,
}

#[derive(Debug, Default)]
pub struct InterfaceDocs {
    pub doc: Option<String>,
    /// Keyed by member name; methods, signals and properties share one namespace here, which
    /// is enough because NetworkManager never reuses a name across them within an interface.
    pub members: BTreeMap<String, MemberDocs>,
}

#[derive(Debug, Default)]
pub struct MemberDocs {
    pub doc: Option<String>,
    /// The type the member's documentation names, for properties and return values.
    pub returns: Option<TypeRef>,
    pub args: BTreeMap<String, ArgDocs>,
}

#[derive(Debug, Default)]
pub struct ArgDocs {
    pub doc: Option<String>,
    pub ty: Option<TypeRef>,
}

/// A type a member's documentation names.
#[derive(Debug, Clone)]
pub struct TypeRef {
    /// The name as written in the header, e.g. `NMDeviceState`.
    pub c_name: String,
}

/// Extract the documentation for one introspection file.
///
/// `known_types` gates which words are accepted as type references, so prose that happens to
/// mention a name cannot invent an annotation.
pub fn extract(xml: &str, known_types: &HashSet<String>) -> Result<Docs> {
    let document = roxmltree::Document::parse(xml).context("parsing the introspection XML")?;
    let mut docs = Docs::default();

    for comment in document.descendants().filter(roxmltree::Node::is_comment) {
        let Some(subject) = documented_element(comment) else {
            continue;
        };
        let Some(name) = subject.attribute("name") else {
            continue;
        };
        let text = comment.text().unwrap_or_default();

        match subject.tag_name().name() {
            "interface" => {
                docs.interfaces.entry(name.to_owned()).or_default().doc = interface_doc(text);
            }
            "property" | "method" | "signal" => {
                // The interface is the one the member sits in, rather than whichever was
                // documented most recently: an interface without a comment of its own would
                // otherwise hand its members to the interface before it.
                let Some(interface) = subject
                    .parent()
                    .filter(|parent| parent.has_tag_name("interface"))
                    .and_then(|parent| parent.attribute("name"))
                else {
                    continue;
                };
                docs.interfaces
                    .entry(interface.to_owned())
                    .or_default()
                    .members
                    .insert(name.to_owned(), parse_member(text, known_types));
            }
            _ => {}
        }
    }

    Ok(docs)
}

/// The element a comment documents: the next one along, if nothing but whitespace separates
/// them.
///
/// A comment that is followed by anything else — another comment, text, the end of its parent —
/// documents nothing, and is left alone rather than attached to whatever comes next.
fn documented_element<'a, 'input>(
    comment: roxmltree::Node<'a, 'input>,
) -> Option<roxmltree::Node<'a, 'input>> {
    let mut next = comment.next_sibling();
    while let Some(node) = next {
        if node.is_text() && node.text().is_some_and(|text| text.trim().is_empty()) {
            next = node.next_sibling();
            continue;
        }
        return node.is_element().then_some(node);
    }
    None
}

/// An interface comment opens with the interface name and a `@short_description:` entry.
fn interface_doc(comment: &str) -> Option<String> {
    let block = Block::parse(comment);
    let short = block.entry("short_description");
    let prose = block.prose();

    let text = match (short, prose) {
        (Some(short), Some(prose)) => format!("{short}\n\n{prose}"),
        (Some(text), None) | (None, Some(text)) => text,
        (None, None) => return None,
    };
    Some(rustdoc_text(&text))
}

fn parse_member(comment: &str, known_types: &HashSet<String>) -> MemberDocs {
    let block = Block::parse(comment);

    let args = block
        .entries
        .iter()
        .filter(|(name, _)| !is_metadata(name))
        .map(|(name, text)| {
            let docs = ArgDocs {
                ty: sole_type_ref(text, known_types).map(|c_name| TypeRef { c_name }),
                doc: Some(rustdoc_text(&strip_leading_type_ref(text))).filter(|d| !d.is_empty()),
            };
            (name.clone(), docs)
        })
        .collect();

    // Most members name their type on a `Returns:` line, but some only describe it in
    // passing — "See NMRadioFlags for the list of flags supported".
    let returns = block
        .entry("Returns")
        .and_then(|text| sole_type_ref(&text, known_types))
        .or_else(|| {
            block
                .prose()
                .and_then(|text| sole_type_ref(&text, known_types))
        })
        .map(|c_name| TypeRef { c_name });

    MemberDocs {
        doc: block.prose().map(|prose| rustdoc_text(&prose)),
        returns,
        args,
    }
}

/// Entries that document the member itself rather than one of its arguments.
fn is_metadata(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "since" | "short_description" | "returns" | "deprecated"
    )
}

/// A gtk-doc comment: a title line, `@name:` entries, then free-form prose.
struct Block {
    entries: Vec<(String, String)>,
    prose: Vec<String>,
}

impl Block {
    fn parse(comment: &str) -> Self {
        let mut entries: Vec<(String, String)> = Vec::new();
        let mut prose: Vec<String> = Vec::new();
        let mut in_entries = true;
        let mut first = true;

        for line in comment.lines() {
            let line = line.trim();

            if line.is_empty() {
                if in_entries && !entries.is_empty() {
                    in_entries = false;
                }
                if !prose.is_empty() {
                    prose.push(String::new());
                }
                continue;
            }

            // The title line names the member and carries no documentation.
            if first {
                first = false;
                if !line.starts_with('@') && line.ends_with(':') {
                    continue;
                }
            }

            if in_entries {
                if let Some(entry) = line.strip_prefix('@') {
                    if let Some((name, text)) = entry.split_once(':') {
                        entries.push((name.trim().to_owned(), text.trim().to_owned()));
                        continue;
                    }
                }
                // `Returns:` is written without the `@` sigil.
                if let Some(text) = line.strip_prefix("Returns:") {
                    entries.push(("Returns".to_owned(), text.trim().to_owned()));
                    continue;
                }
                if let Some((_, text)) = entries.last_mut() {
                    text.push(' ');
                    text.push_str(line);
                    continue;
                }
                in_entries = false;
            }

            // `Returns:` also shows up after the prose has started.
            if let Some(text) = line.strip_prefix("Returns:") {
                entries.push(("Returns".to_owned(), text.trim().to_owned()));
                continue;
            }
            prose.push(line.to_owned());
        }

        Self { entries, prose }
    }

    fn entry(&self, name: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, text)| text.clone())
    }

    fn prose(&self) -> Option<String> {
        let text = self.prose.join("\n").trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

/// The one NetworkManager type `text` refers to, if it refers to exactly one.
///
/// Ambiguous references — `Returns: A tuple of NMDeviceState and NMDeviceStateReason` — are
/// left alone rather than resolved to whichever came first.
fn sole_type_ref(text: &str, known_types: &HashSet<String>) -> Option<String> {
    let mut found: Option<&str> = None;
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if !known_types.contains(word) {
            continue;
        }
        match found {
            Some(first) if first != word => return None,
            _ => found = Some(word),
        }
    }
    found.map(str::to_owned)
}

/// Drop the leading `(NMSomeType)` from an argument's documentation, which the generated
/// signature now states outright.
fn strip_leading_type_ref(text: &str) -> String {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix('(') else {
        return text.to_owned();
    };
    match rest.find(')') {
        Some(end) => rest[end + 1..].trim_start().to_owned(),
        None => text.to_owned(),
    }
}

/// Turn a gtk-doc/DocBook fragment into plain text fit for a Rust doc comment.
pub fn rustdoc_text(text: &str) -> String {
    let text = strip_markup(text);
    let text = decode_entities(&text);
    // gtk-doc sigils: `#NMDeviceState` for types, `%NM_DEVICE_STATE_UNKNOWN` for constants.
    let text = text
        .replace("#NM", "NM")
        .replace("%NM", "NM")
        .replace("%TRUE", "true")
        .replace("%FALSE", "false")
        .replace("%NULL", "unset");

    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank > 0 {
                out.push('\n');
            }
        }
        blank = 0;
        out.push_str(&for_rustdoc(line));
    }
    out
}

/// Escape the prose so that rustdoc reads it as prose.
///
/// NetworkManager writes things like "'unmanaged' in a [device*] section" and bare issue URLs,
/// which rustdoc would otherwise take for a broken intra-doc link and an unlinked URL.
fn for_rustdoc(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for word in line.split_inclusive(char::is_whitespace) {
        let trimmed = word.trim_end();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            out.push('<');
            out.push_str(trimmed);
            out.push('>');
            out.push_str(&word[trimmed.len()..]);
            continue;
        }
        for ch in word.chars() {
            if ch == '[' || ch == ']' {
                out.push('\\');
            }
            out.push(ch);
        }
    }
    out
}

/// Remove DocBook and `<link>` markup, keeping paragraph breaks.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(len) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tag = &rest[start + 1..start + len];
        let name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if matches!(
            name,
            "para" | "variablelist" | "varlistentry" | "listitem" | "term" | "programlisting"
        ) {
            out.push('\n');
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> HashSet<String> {
        ["NMDeviceState", "NMDeviceStateReason", "NMDeviceManaged"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    const XML: &str = r#"
<node name="/">
  <!--
      org.freedesktop.NetworkManager.Device:
      @short_description: Device.
  -->
  <interface name="org.freedesktop.NetworkManager.Device">
    <!--
        State:

        The current state of the device.

        Returns: <link linkend="NMDeviceState">NMDeviceState</link>
    -->
    <property name="State" type="u" access="read"/>

    <!--
        StateReason:

        Returns: A tuple of <link linkend="NMDeviceState">NMDeviceState</link> and
        <link linkend="NMDeviceStateReason">NMDeviceStateReason</link>.
    -->
    <property name="StateReason" type="(uu)" access="read"/>

    <!--
        SetManaged:
        @managed:(<link linkend="NMDeviceManaged">NMDeviceManaged</link>) Whether it is managed.
        @since: 1.58

        Set the managed state.
    -->
    <method name="SetManaged">
      <arg name="managed" type="u" direction="in"/>
    </method>
  </interface>
</node>
"#;

    #[test]
    fn reads_property_types_and_prose() {
        let docs = extract(XML, &known()).unwrap();
        let iface = &docs.interfaces["org.freedesktop.NetworkManager.Device"];
        assert_eq!(iface.doc.as_deref(), Some("Device."));

        let state = &iface.members["State"];
        assert_eq!(
            state.returns.as_ref().map(|r| r.c_name.as_str()),
            Some("NMDeviceState")
        );
        assert_eq!(
            state.doc.as_deref(),
            Some("The current state of the device.")
        );
    }

    #[test]
    fn leaves_ambiguous_references_alone() {
        let docs = extract(XML, &known()).unwrap();
        let iface = &docs.interfaces["org.freedesktop.NetworkManager.Device"];
        assert!(iface.members["StateReason"].returns.is_none());
    }

    #[test]
    fn reads_argument_types_and_skips_metadata() {
        let docs = extract(XML, &known()).unwrap();
        let method =
            &docs.interfaces["org.freedesktop.NetworkManager.Device"].members["SetManaged"];
        assert_eq!(method.doc.as_deref(), Some("Set the managed state."));
        assert!(!method.args.contains_key("since"));

        let managed = &method.args["managed"];
        assert_eq!(
            managed.ty.as_ref().map(|r| r.c_name.as_str()),
            Some("NMDeviceManaged")
        );
        assert_eq!(managed.doc.as_deref(), Some("Whether it is managed."));
    }

    /// A member belongs to the interface it sits in, not to the last one documented.
    ///
    /// Every interface NetworkManager ships has a comment of its own, so nothing in the
    /// vendored spec tells these two rules apart — which is exactly why it is worth pinning.
    #[test]
    fn gives_a_member_to_the_interface_it_sits_in() {
        const TWO: &str = r#"
<node name="/">
  <!--
      org.freedesktop.NetworkManager.Device:
      @short_description: Device.
  -->
  <interface name="org.freedesktop.NetworkManager.Device">
    <!--
        State:

        The current state of the device.
    -->
    <property name="State" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Undocumented">
    <!--
        Other:

        Something else entirely.
    -->
    <property name="Other" type="u" access="read"/>
  </interface>
</node>
"#;
        let docs = extract(TWO, &known()).unwrap();
        let device = &docs.interfaces["org.freedesktop.NetworkManager.Device"];
        assert!(device.members.contains_key("State"));
        assert!(!device.members.contains_key("Other"));

        let other = &docs.interfaces["org.freedesktop.NetworkManager.Undocumented"];
        assert_eq!(
            other.members["Other"].doc.as_deref(),
            Some("Something else entirely.")
        );
        assert!(other.doc.is_none());
    }

    /// A comment followed by another comment documents nothing.
    #[test]
    fn leaves_a_comment_with_no_element_after_it_alone() {
        const STRAY: &str = r#"
<node name="/">
  <!--
      org.freedesktop.NetworkManager.Device:
      @short_description: Device.
  -->
  <interface name="org.freedesktop.NetworkManager.Device">
    <!-- A note about what follows, documenting nothing. -->
    <!--
        State:

        The current state of the device.
    -->
    <property name="State" type="u" access="read"/>
  </interface>
</node>
"#;
        let docs = extract(STRAY, &known()).unwrap();
        let iface = &docs.interfaces["org.freedesktop.NetworkManager.Device"];
        assert_eq!(iface.members.len(), 1);
        assert_eq!(
            iface.members["State"].doc.as_deref(),
            Some("The current state of the device.")
        );
    }
}
