//! Reader for `NM-1.0.gir`, NetworkManager's GObject introspection data.
//!
//! This replaces reading the C headers. Everything that made that fragile is already resolved
//! here by the introspection scanner:
//!
//! * a flag set is a `<bitfield>` element rather than a `/*< flags >*/` comment;
//! * values are integers, not C expressions over private constants;
//! * member names are the short nicks, so there is no shared prefix to strip;
//! * documentation is in `<doc>` elements rather than gtk-doc comment blocks;
//! * members upstream does not consider API are already absent.
//!
//! It also covers the whole namespace in one file, so there is no list of headers to keep in
//! step with wherever NetworkManager happens to declare a type.

use anyhow::{Context, Result, bail};

/// Whether a type enumerates alternatives or combines independent bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Enum,
    Flags,
}

#[derive(Debug, Clone)]
pub struct Variant {
    /// The name as written in the C header, e.g. `NM_DEVICE_STATE_ACTIVATED`.
    pub c_name: String,
    /// The introspection name, e.g. `activated`.
    pub nick: String,
    pub value: i128,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    /// The name as written in the C header, e.g. `NMDeviceState`.
    pub c_name: String,
    /// The name to generate, e.g. `DeviceState`.
    pub rust_name: String,
    pub kind: Kind,
    pub doc: Option<String>,
    pub deprecated: bool,
    pub variants: Vec<Variant>,
}

impl TypeDef {
    pub fn is_flags(&self) -> bool {
        self.kind == Kind::Flags
    }

    /// The variants that can become distinct Rust items, each with the name it takes.
    ///
    /// NetworkManager ends some flag sets with an alias of the last real bit; a repeated value
    /// or name is dropped in favour of the one it duplicates, since neither a second Rust item
    /// nor a second `<tp:enumvalue>` could carry it.
    pub fn rust_variants(&self) -> Vec<(&Variant, String)> {
        let prefix = shared_prefix(&self.variants);
        let mut seen_values = Vec::new();
        let mut seen_names: Vec<String> = Vec::new();
        let mut out = Vec::new();

        for variant in &self.variants {
            let name = if self.is_flags() {
                variant.flag_name(&prefix)
            } else {
                variant.variant_name(&prefix)
            };
            if seen_values.contains(&variant.value) || seen_names.contains(&name) {
                continue;
            }
            seen_values.push(variant.value);
            seen_names.push(name.clone());
            out.push((variant, name));
        }
        out
    }
}

const CORE_NS: &str = "http://www.gtk.org/introspection/core/1.0";
const C_NS: &str = "http://www.gtk.org/introspection/c/1.0";

/// Read every enumeration and flag set from the introspection data.
pub fn parse(source: &str) -> Result<Vec<TypeDef>> {
    let document = roxmltree::Document::parse(source).context("parsing NM-1.0.gir")?;
    let namespace = document
        .descendants()
        .find(|node| node.has_tag_name((CORE_NS, "namespace")))
        .context("NM-1.0.gir has no <namespace>")?;

    let mut defs = Vec::new();
    for node in namespace.children() {
        let kind = match node.tag_name().name() {
            "enumeration" if node.tag_name().namespace() == Some(CORE_NS) => Kind::Enum,
            "bitfield" if node.tag_name().namespace() == Some(CORE_NS) => Kind::Flags,
            _ => continue,
        };

        // The C name is what NetworkManager's own documentation calls the type, and what the
        // introspection XML of the D-Bus interfaces refers to.
        let c_name = node
            .attribute((C_NS, "type"))
            .with_context(|| format!("a <{}> without a c:type", node.tag_name().name()))?;

        let mut variants = Vec::new();
        for member in node
            .children()
            .filter(|c| c.has_tag_name((CORE_NS, "member")))
        {
            let nick = member
                .attribute("name")
                .with_context(|| format!("a member of `{c_name}` has no name"))?;
            let raw = member
                .attribute("value")
                .with_context(|| format!("`{c_name}.{nick}` has no value"))?;
            let value = raw
                .parse::<i128>()
                .with_context(|| format!("`{c_name}.{nick}` has a non-integer value `{raw}`"))?;

            variants.push(Variant {
                c_name: member
                    .attribute((C_NS, "identifier"))
                    .unwrap_or(nick)
                    .to_owned(),
                nick: nick.to_owned(),
                value,
                doc: doc_of(member),
            });
        }
        if variants.is_empty() {
            continue;
        }

        defs.push(TypeDef {
            rust_name: rust_name(c_name),
            c_name: c_name.to_owned(),
            kind,
            doc: doc_of(node),
            deprecated: node.attribute("deprecated") == Some("1"),
            variants,
        });
    }

    if defs.is_empty() {
        bail!("no <enumeration> or <bitfield> found — has the introspection format changed?");
    }
    Ok(defs)
}

fn doc_of(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let doc = node.children().find(|c| c.has_tag_name((CORE_NS, "doc")))?;
    let text = doc.text()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Drop NetworkManager's `NM` namespace prefix, which the crate name already supplies.
///
/// A few types (`NM80211ApFlags`) would start with a digit without it and keep it.
fn rust_name(c_name: &str) -> String {
    match c_name.strip_prefix("NM") {
        Some(stripped) if stripped.starts_with(|c: char| c.is_ascii_alphabetic()) => {
            stripped.to_owned()
        }
        _ => c_name.to_owned(),
    }
}

impl Variant {
    /// The Rust name for this variant of an enumeration, e.g. `activated` -> `Activated`.
    ///
    /// A nick that cannot open a Rust identifier — `6lowpan` — falls back to the C name with
    /// the prefix its siblings share removed, which is how it reads in NetworkManager's own
    /// documentation (`NM_DEVICE_TYPE_6LOWPAN` -> `Type6LOWPAN`).
    pub fn variant_name(&self, shared_prefix: &str) -> String {
        let pascal = pascal_case(&self.nick);
        if pascal.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            return pascal;
        }
        let tail = &self.c_name[shared_prefix.len().min(self.c_name.len())..];
        screaming_to_pascal(tail)
    }

    /// The Rust name for this bit of a flag set, e.g. `carrier_detect` -> `CARRIER_DETECT`.
    pub fn flag_name(&self, shared_prefix: &str) -> String {
        let upper = self.nick.replace('-', "_").to_ascii_uppercase();
        if upper.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            return upper;
        }
        self.c_name[shared_prefix.len().min(self.c_name.len())..].to_owned()
    }
}

/// The prefix every variant's C name shares, cut back to an underscore.
///
/// Only used to name the odd variant whose nick cannot be an identifier.
pub fn shared_prefix(variants: &[Variant]) -> String {
    let shared = variants
        .iter()
        .map(|variant| variant.c_name.as_str())
        .reduce(|a, b| {
            let len = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            &a[..len]
        })
        .unwrap_or_default();

    // Back off a word at a time until what is left can open an identifier.
    let mut prefix = &shared[..shared.rfind('_').map_or(0, |i| i + 1)];
    while variants.iter().any(|variant| {
        !variant.c_name[prefix.len().min(variant.c_name.len())..]
            .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
    }) {
        match prefix[..prefix.len().saturating_sub(1)].rfind('_') {
            Some(previous) => prefix = &prefix[..previous + 1],
            None => return String::new(),
        }
    }
    prefix.to_owned()
}

/// `carrier_detect` -> `CarrierDetect`, `unused1` -> `Unused1`.
fn pascal_case(nick: &str) -> String {
    let mut out = String::with_capacity(nick.len());
    for word in nick.split(['_', '-']).filter(|w| !w.is_empty()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// `TYPE_6LOWPAN` -> `Type6LOWPAN`: words carrying digits are kept as written.
fn screaming_to_pascal(name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    const GIR: &str = r#"<?xml version="1.0"?>
<repository xmlns="http://www.gtk.org/introspection/core/1.0"
            xmlns:c="http://www.gtk.org/introspection/c/1.0">
  <namespace name="NM">
    <enumeration name="DeviceType" c:type="NMDeviceType">
      <doc xml:space="preserve">The type of hardware.</doc>
      <member name="ethernet" value="1" c:identifier="NM_DEVICE_TYPE_ETHERNET">
        <doc xml:space="preserve">an Ethernet device</doc>
      </member>
      <member name="unused1" value="3" c:identifier="NM_DEVICE_TYPE_UNUSED1"/>
      <member name="6lowpan" value="28" c:identifier="NM_DEVICE_TYPE_6LOWPAN"/>
      <member name="wifi_p2p" value="30" c:identifier="NM_DEVICE_TYPE_WIFI_P2P"/>
    </enumeration>
    <bitfield name="DeviceCapabilities" c:type="NMDeviceCapabilities">
      <member name="carrier_detect" value="2" c:identifier="NM_DEVICE_CAP_CARRIER_DETECT"/>
    </bitfield>
  </namespace>
</repository>"#;

    #[test]
    fn reads_kinds_values_and_docs() {
        let defs = parse(GIR).unwrap();
        assert_eq!(defs.len(), 2);

        let device_type = &defs[0];
        assert_eq!(device_type.rust_name, "DeviceType");
        assert_eq!(device_type.kind, Kind::Enum);
        assert_eq!(device_type.doc.as_deref(), Some("The type of hardware."));
        assert_eq!(device_type.variants[0].value, 1);
        assert_eq!(
            device_type.variants[0].doc.as_deref(),
            Some("an Ethernet device")
        );

        assert_eq!(defs[1].kind, Kind::Flags);
    }

    #[test]
    fn names_variants_from_their_nicks() {
        let defs = parse(GIR).unwrap();
        let prefix = shared_prefix(&defs[0].variants);
        let names: Vec<_> = defs[0]
            .variants
            .iter()
            .map(|v| v.variant_name(&prefix))
            .collect();
        assert_eq!(names, ["Ethernet", "Unused1", "Type6LOWPAN", "WifiP2p"]);
    }

    #[test]
    fn names_flags_in_screaming_case() {
        let defs = parse(GIR).unwrap();
        let prefix = shared_prefix(&defs[1].variants);
        assert_eq!(defs[1].variants[0].flag_name(&prefix), "CARRIER_DETECT");
    }
}
