//! Working out what kind of object an object path points at.
//!
//! NetworkManager's introspection says only `type="o"`. Which interface the object at the
//! other end implements is sometimes named in the documentation, more often only implied by
//! the member's own name — `Ip4Config` is an `IP4Config`, `Devices` is a list of `Device` —
//! and sometimes neither, at which point `annotations.toml` has to say.
//!
//! Nothing here guesses. A member whose target cannot be established keeps a plain object
//! path, which is correct if unhelpful; `Connection.Active.SpecificObject` genuinely has no
//! single answer, since it is an access point or a Wi-Fi P2P peer depending on the connection.

use std::collections::BTreeMap;

use zbus_xml::{Interface, Node};

use crate::annotate::Docs;
use crate::render::Site;

/// The D-Bus signature of a single object path, and of a list of them.
pub const PATH: &str = "o";
pub const PATH_ARRAY: &str = "ao";

/// Which interface each object-path member points at, keyed by interface then site.
pub type Targets = BTreeMap<String, BTreeMap<Site, String>>;

/// Resolve every object-path member across the whole spec.
///
/// `overrides` supplies the targets that neither the documentation nor the member's name gives.
pub fn resolve(
    parsed: &[(String, Node<'_>, Docs)],
    interfaces: &[String],
    overrides: &[(String, String, String)],
) -> Targets {
    let index = NameIndex::build(interfaces);
    let mut targets: Targets = BTreeMap::new();

    for (_, node, docs) in parsed {
        for interface in node.interfaces() {
            let name = interface.name().to_string();
            let sites = interface_targets(interface, docs, &index, overrides);
            if !sites.is_empty() {
                targets.entry(name).or_default().extend(sites);
            }
        }
    }
    targets
}

fn interface_targets(
    interface: &Interface<'_>,
    docs: &Docs,
    index: &NameIndex,
    overrides: &[(String, String, String)],
) -> BTreeMap<Site, String> {
    let interface_name = interface.name().to_string();
    let iface_docs = docs.interfaces.get(interface_name.as_str());
    let mut out = BTreeMap::new();

    let mut record = |site: Site, member: &str, signature: &str, doc: Option<&str>| {
        if signature != PATH && signature != PATH_ARRAY {
            return;
        }
        let key = match &site {
            Site::Property(property) => property.clone(),
            Site::Arg(member, arg) => format!("{member}.{arg}"),
        };
        let target = overrides
            .iter()
            .find(|(iface, name, _)| *iface == interface_name && *name == key)
            .map(|(_, _, target)| target.clone())
            .or_else(|| infer(member, doc, index));
        if let Some(target) = target {
            out.insert(site, target);
        }
    };

    for property in interface.properties() {
        let name = property.name().to_string();
        let doc = iface_docs
            .and_then(|i| i.members.get(&name))
            .and_then(|m| m.doc.as_deref());
        record(
            Site::Property(name.clone()),
            &name,
            &property.ty().to_string(),
            doc,
        );
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

    for (member, args) in members {
        for arg in args {
            let Some(arg_name) = arg.name() else { continue };
            let doc = iface_docs
                .and_then(|i| i.members.get(&member))
                .and_then(|m| m.args.get(arg_name))
                .and_then(|a| a.doc.as_deref());
            record(
                Site::Arg(member.clone(), arg_name.to_owned()),
                arg_name,
                &arg.ty().inner().to_string(),
                doc,
            );
        }
    }

    out
}

/// The interface a member points at, from its documentation or its name.
fn infer(member: &str, doc: Option<&str>, index: &NameIndex) -> Option<String> {
    // Strongest evidence: the documentation names the interface outright, as
    // `Device.ActiveConnection` does.
    if let Some(doc) = doc
        && let Some(named) = index.named_in(doc)
    {
        return Some(named);
    }

    // Otherwise the member's own name: `Devices` is a list of `Device`, `Ip4Config` is an
    // `IP4Config`, and `device_path` is a `Device`. Only an unambiguous match counts, so
    // `Connections` — which could be the settings interface or the VPN one — is refused.
    let normalised = normalise(member);
    let forms = [
        Some(normalised.clone()),
        normalised.strip_suffix('s').map(str::to_owned),
        normalised.strip_suffix("path").map(str::to_owned),
    ];
    forms
        .into_iter()
        .flatten()
        .filter(|candidate| !candidate.is_empty())
        .find_map(|candidate| index.unique(&candidate))
}

/// The interface names of the spec, in the forms a member name might match.
struct NameIndex {
    /// Normalised name to the interfaces that could be meant by it.
    by_name: BTreeMap<String, Vec<String>>,
    all: Vec<String>,
}

impl NameIndex {
    fn build(interfaces: &[String]) -> Self {
        let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for interface in interfaces {
            let tail = interface
                .strip_prefix("org.freedesktop.NetworkManager")
                .unwrap_or(interface)
                .trim_start_matches('.');
            if tail.is_empty() {
                continue;
            }
            // Both the whole tail (`Connection.Active`) and its last segment (`Active`), since
            // a member could be named after either.
            for form in [tail, tail.rsplit('.').next().unwrap_or(tail)] {
                let entry = by_name.entry(normalise(form)).or_default();
                if !entry.contains(interface) {
                    entry.push(interface.clone());
                }
            }
        }
        Self {
            by_name,
            all: interfaces.to_vec(),
        }
    }

    /// The interface this name can only mean, if there is exactly one.
    fn unique(&self, name: &str) -> Option<String> {
        match self.by_name.get(name)?.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }

    /// The interface a piece of documentation names in full, if it names one.
    fn named_in(&self, doc: &str) -> Option<String> {
        doc.split_whitespace()
            .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.'))
            .find(|word| self.all.iter().any(|interface| interface == word))
            .map(str::to_owned)
    }
}

/// Compare names ignoring case and punctuation: `Ip4Config` and `IP4Config` are the same name.
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> NameIndex {
        NameIndex::build(&[
            "org.freedesktop.NetworkManager".to_owned(),
            "org.freedesktop.NetworkManager.Device".to_owned(),
            "org.freedesktop.NetworkManager.IP4Config".to_owned(),
            "org.freedesktop.NetworkManager.AccessPoint".to_owned(),
            "org.freedesktop.NetworkManager.Connection.Active".to_owned(),
            "org.freedesktop.NetworkManager.Settings.Connection".to_owned(),
            "org.freedesktop.NetworkManager.VPN.Connection".to_owned(),
        ])
    }

    #[test]
    fn infers_from_the_member_name() {
        let index = index();
        assert_eq!(
            infer("Ip4Config", None, &index).as_deref(),
            Some("org.freedesktop.NetworkManager.IP4Config")
        );
        assert_eq!(
            infer("Devices", None, &index).as_deref(),
            Some("org.freedesktop.NetworkManager.Device")
        );
        assert_eq!(
            infer("AccessPoints", None, &index).as_deref(),
            Some("org.freedesktop.NetworkManager.AccessPoint")
        );
    }

    #[test]
    fn prefers_what_the_documentation_names() {
        let index = index();
        let doc = "Object path of an ActiveConnection object that implements the \
                   org.freedesktop.NetworkManager.Connection.Active D-Bus interface.";
        assert_eq!(
            infer("ActiveConnection", Some(doc), &index).as_deref(),
            Some("org.freedesktop.NetworkManager.Connection.Active")
        );
    }

    #[test]
    fn infers_through_a_path_suffix() {
        assert_eq!(
            infer("device_path", None, &index()).as_deref(),
            Some("org.freedesktop.NetworkManager.Device")
        );
    }

    #[test]
    fn refuses_an_ambiguous_name() {
        // `Connection` could be the settings one or the VPN one, so it needs an override.
        assert_eq!(infer("Connections", None, &index()), None);
        assert_eq!(infer("Parent", None, &index()), None);
    }
}
