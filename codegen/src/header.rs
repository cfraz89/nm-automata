//! Parser for `nm-dbus-interface.h`.
//!
//! NetworkManager sends enumerations and flag sets over D-Bus as bare integers and documents
//! them nowhere else, so the C header is the only machine-readable source for their values.
//! It is regular enough to read directly: every type is a `typedef enum` preceded by a
//! gtk-doc block, with `/*< flags >*/` marking the bit sets.

use anyhow::{Context, Result, bail};

/// Whether a type enumerates alternatives or combines independent bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Enum,
    Flags,
}

#[derive(Debug, Clone)]
pub struct Variant {
    /// The name as written in the header, e.g. `NM_DEVICE_STATE_ACTIVATED`.
    pub c_name: String,
    /// `c_name` with the prefix shared by the type's variants removed, e.g. `ACTIVATED`.
    pub suffix: String,
    pub value: i128,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    /// The name as written in the header, e.g. `NMDeviceState`.
    pub c_name: String,
    /// The name to generate, e.g. `DeviceState`.
    pub rust_name: String,
    pub kind: Kind,
    pub doc: Option<String>,
    pub variants: Vec<Variant>,
}

impl TypeDef {
    pub fn is_flags(&self) -> bool {
        self.kind == Kind::Flags
    }
}

/// Parse every `typedef enum` in `src`.
pub fn parse(src: &str) -> Result<Vec<TypeDef>> {
    let mut defs = Vec::new();
    let mut cursor = 0;

    while let Some(offset) = src[cursor..].find("typedef enum") {
        let start = cursor + offset;
        let open = src[start..]
            .find('{')
            .map(|i| start + i)
            .context("`typedef enum` without a body")?;
        // Enumeration bodies never nest, so the first closing brace is the matching one.
        let close = src[open..]
            .find('}')
            .map(|i| open + i)
            .context("unterminated `typedef enum` body")?;
        let semicolon = src[close..]
            .find(';')
            .map(|i| close + i)
            .context("`typedef enum` without a trailing name")?;

        let body = &src[open + 1..close];
        let c_name = src[close + 1..semicolon].trim();
        cursor = semicolon + 1;

        // Anonymous enumerations are not part of the API surface.
        if !is_ident(c_name) {
            continue;
        }

        // GObject-introspection annotations sit either between `enum` and `{` or just inside
        // the body, ahead of the first variant.
        let leading = body
            .find("NM_")
            .map_or(body, |first_variant| &body[..first_variant]);
        let annotations = format!("{} {leading}", &src[start..open]);

        let doc = preceding_doc_block(src, start);
        let (type_doc, variant_docs) = split_doc_block(doc.as_deref());

        let mut variants = parse_variants(body, &variant_docs)
            .with_context(|| format!("parsing the values of `{c_name}`"))?;
        if variants.is_empty() {
            continue;
        }
        assign_suffixes(&mut variants);

        defs.push(TypeDef {
            rust_name: rust_name(c_name),
            c_name: c_name.to_owned(),
            kind: if annotations.contains("flags") {
                Kind::Flags
            } else {
                Kind::Enum
            },
            doc: type_doc,
            variants,
        });
    }

    if defs.is_empty() {
        bail!("no `typedef enum` found — has the header layout changed?");
    }
    Ok(defs)
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

/// Parse the `NAME = VALUE,` entries of an enumeration body.
///
/// Anything beyond an integer literal, a reference to an earlier variant of the same type, or
/// an implicit (auto-incrementing) value is rejected rather than guessed at, so that a future
/// header using richer expressions fails the build instead of generating wrong constants.
fn parse_variants(body: &str, docs: &[(String, String)]) -> Result<Vec<Variant>> {
    let skipped = skipped_variants(body);
    let stripped = strip_comments(body);
    let mut variants: Vec<Variant> = Vec::new();
    // `None` once a skipped variant has made the running count unknowable.
    let mut next_implicit = Some(0i128);

    for entry in stripped.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let (name, expr) = match entry.split_once('=') {
            Some((name, expr)) => (name.trim(), Some(expr.trim())),
            None => (entry, None),
        };
        if !is_ident(name) {
            bail!("`{name}` is not a variant name");
        }

        // Variants NetworkManager marks `/*< skip >*/` are internal bookkeeping, not API. They
        // are also the ones whose values are written as expressions over private constants,
        // so they are dropped before any attempt to evaluate them.
        if skipped.iter().any(|s| s == name) {
            next_implicit = None;
            continue;
        }

        let value = match expr {
            Some(expr) => parse_value(expr, &variants)
                .with_context(|| format!("unsupported value `{expr}` for `{name}`"))?,
            None => next_implicit.with_context(|| {
                format!(
                    "`{name}` takes an implicit value, but a skipped variant before it \
                         means the count is unknown"
                )
            })?,
        };
        next_implicit = Some(value + 1);

        variants.push(Variant {
            doc: docs
                .iter()
                .find(|(doc_name, _)| doc_name == name)
                .map(|(_, doc)| doc.clone()),
            c_name: name.to_owned(),
            suffix: String::new(),
            value,
        });
    }

    Ok(variants)
}

/// The variants annotated `/*< skip >*/`, which upstream excludes from its own bindings.
///
/// The annotation trails the variant it applies to, on the same line, so the variant is the
/// first name on that line.
fn skipped_variants(body: &str) -> Vec<String> {
    body.lines()
        .filter(|line| {
            line.split("/*<")
                .skip(1)
                .filter_map(|rest| rest.split("*/").next())
                .any(|annotation| annotation.contains("skip"))
        })
        .filter_map(|line| {
            let name: String = line
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            is_ident(&name).then_some(name)
        })
        .collect()
}

fn parse_value(expr: &str, previous: &[Variant]) -> Option<i128> {
    if let Some(hex) = expr.strip_prefix("0x").or_else(|| expr.strip_prefix("0X")) {
        return i128::from_str_radix(hex, 16).ok();
    }
    if let Ok(value) = expr.parse::<i128>() {
        return Some(value);
    }
    // Aliases such as `NM_SECRET_AGENT_CAPABILITY_LAST = NM_SECRET_AGENT_CAPABILITY_VPN_HINTS`.
    previous
        .iter()
        .find(|variant| variant.c_name == expr)
        .map(|variant| variant.value)
}

/// Give every variant the name it will carry in Rust, by removing the prefix they all share.
///
/// The prefix is cut back to an underscore boundary, since two variants where one name is a
/// prefix of the other (`NM_FOO_BAR`, `NM_FOO_BAR_BAZ`) would otherwise share more than a
/// whole word. A variant that would be left starting with a digit — `NM_DEVICE_TYPE_6LOWPAN`
/// among plain ones like `NM_DEVICE_TYPE_ETHERNET` — keeps one more segment of the prefix, so
/// that the odd name out does not have to be paid for by all the others.
fn assign_suffixes(variants: &mut [Variant]) {
    let shared = variants
        .iter()
        .map(|variant| variant.c_name.as_str())
        .reduce(common_prefix)
        .unwrap_or_default();
    let prefix = shared[..shared.rfind('_').map_or(0, |i| i + 1)].to_owned();

    for variant in variants {
        let mut start = prefix.len();
        while !variant.c_name[start..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            match variant.c_name[..start.saturating_sub(1)].rfind('_') {
                Some(previous) => start = previous + 1,
                None => {
                    start = 0;
                    break;
                }
            }
        }
        variant.suffix = variant.c_name[start..].to_owned();
    }
}

fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let len = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    &a[..len]
}

/// The gtk-doc block immediately above `offset`, if there is one.
fn preceding_doc_block(src: &str, offset: usize) -> Option<String> {
    let before = &src[..offset];
    // Only whitespace may separate the block from the declaration it documents.
    let end = before.trim_end().strip_suffix("*/")?.len();
    let start = before[..end].rfind("/**")?;
    Some(before[start + 3..end].to_owned())
}

/// Split a gtk-doc block into the type's own documentation and its per-variant documentation.
///
/// The block opens with `Name:`, continues with one `@VARIANT: …` entry per value, and ends
/// with free-form prose after a blank line.
fn split_doc_block(block: Option<&str>) -> (Option<String>, Vec<(String, String)>) {
    let Some(block) = block else {
        return (None, Vec::new());
    };

    let mut variants: Vec<(String, String)> = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut in_variants = true;
    let mut seen_title = false;

    for line in block.lines().map(strip_comment_margin) {
        if line.is_empty() {
            // The blank line that follows the `@` entries starts the prose. Blank lines
            // before them — the block opens with one — are just layout.
            if !variants.is_empty() {
                in_variants = false;
            }
            if !prose.is_empty() {
                prose.push("");
            }
            continue;
        }

        // The block opens by naming the type, which the declaration already says.
        if !seen_title {
            seen_title = true;
            if !line.starts_with('@') && line.ends_with(':') {
                continue;
            }
        }

        if in_variants {
            if let Some(entry) = line.strip_prefix('@') {
                if let Some((name, text)) = entry.split_once(':') {
                    variants.push((name.trim().to_owned(), text.trim().to_owned()));
                    continue;
                }
            }
            // A continuation of the previous `@` entry — or, when the type documents no
            // variants at all, the start of the prose.
            if let Some((_, text)) = variants.last_mut() {
                text.push(' ');
                text.push_str(line.trim_start());
                continue;
            }
            in_variants = false;
        }
        prose.push(line);
    }

    let prose = prose.join("\n").trim().to_owned();
    let doc = (!prose.is_empty()).then_some(prose);
    (doc, variants)
}

/// Remove the ` * ` that opens every line of a C comment block.
fn strip_comment_margin(line: &str) -> &str {
    let line = line.trim();
    let line = line.trim_start_matches('*');
    line.strip_prefix(' ').unwrap_or(line).trim_end()
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
/**
 * NMDeviceState:
 * @NM_DEVICE_STATE_UNKNOWN: the device's state is unknown
 * @NM_DEVICE_STATE_ACTIVATED: the device has a network connection,
 *   either local or global.
 *
 * Describes a device.
 **/
typedef enum {
    NM_DEVICE_STATE_UNKNOWN   = 0,
    NM_DEVICE_STATE_ACTIVATED = 100,
} NMDeviceState;

/**
 * NM80211ApFlags:
 * @NM_802_11_AP_FLAGS_NONE: null capability
 **/
typedef enum /*< underscore_name=nm_802_11_ap_flags, flags >*/ {
    NM_802_11_AP_FLAGS_NONE    = 0x00000000,
    NM_802_11_AP_FLAGS_PRIVACY = 0x00000001,
    NM_802_11_AP_FLAGS_LAST    = NM_802_11_AP_FLAGS_PRIVACY,
} NM80211ApFlags;

typedef enum {
    NM_IMPLICIT_A,
    NM_IMPLICIT_B,
} NMImplicit;
"#;

    #[test]
    fn reads_values_docs_and_kinds() {
        let defs = parse(SAMPLE).unwrap();
        assert_eq!(defs.len(), 3);

        let state = &defs[0];
        assert_eq!(state.rust_name, "DeviceState");
        assert_eq!(state.kind, Kind::Enum);
        assert_eq!(state.doc.as_deref(), Some("Describes a device."));
        assert_eq!(state.variants[1].suffix, "ACTIVATED");
        assert_eq!(state.variants[1].value, 100);
        assert_eq!(
            state.variants[1].doc.as_deref(),
            Some("the device has a network connection, either local or global.")
        );
    }

    #[test]
    fn keeps_names_that_would_start_with_a_digit() {
        let defs = parse(SAMPLE).unwrap();
        assert_eq!(defs[1].rust_name, "NM80211ApFlags");
        assert_eq!(defs[1].kind, Kind::Flags);
        assert_eq!(defs[1].variants[0].suffix, "NONE");
    }

    #[test]
    fn resolves_aliases_and_implicit_values() {
        let defs = parse(SAMPLE).unwrap();
        assert_eq!(defs[1].variants[2].value, 1);
        assert_eq!(defs[2].variants[0].value, 0);
        assert_eq!(defs[2].variants[1].value, 1);
    }
}
