//! Emitters: the accessors written beside a generated proxy, and the Rust modules of
//! NetworkManager's types, connection profiles, object paths and dictionary shapes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse_quote;
use zbus_xml::{ArgDirection, Interface, Node};

use anyhow::Context as _;

use crate::annotate::{Docs, rustdoc_text};
use crate::gir::TypeDef;
use crate::proxy::{MemberSig, Named, doc_line};

/// A NetworkManager type as it is used on the wire, resolved across the whole spec.
#[derive(Debug, Clone)]
pub struct Resolved<'a> {
    pub def: &'a TypeDef,
    /// The D-Bus signature the type travels as, e.g. `u`.
    pub signature: String,
    /// The Rust integer backing it, e.g. `u32`.
    pub repr: &'static str,
}

/// Every member of `node` that could carry a dictionary, and the signature it carries.
///
/// A property is named `Interface.Property` and an argument `Interface.Member.argument`, which
/// is how `annotations.toml` names them in a `[[vardict]]`'s `applies`.
pub fn dictionary_sites(node: &Node<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for interface in node.interfaces() {
        let name = interface.name().to_string();
        for property in interface.properties() {
            out.push((
                format!("{name}.{}", property.name()),
                property.ty().to_string(),
            ));
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
                let Some(argument) = arg.name() else { continue };
                out.push((
                    format!("{name}.{member}.{argument}"),
                    arg.ty().inner().to_string(),
                ));
            }
        }
    }
    out
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
        if let Some(reference) = member.returns.as_ref() {
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
            if let Some(reference) = annotated {
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

/// The enumeration a site is read as, if the named type really is what travels there.
///
/// A mismatch means the comment and the signature disagree — the annotation is dropped and
/// the member keeps its plain integer type rather than failing to decode at runtime.
fn reference_for(candidate: &Candidate, types: &BTreeMap<String, Resolved<'_>>) -> Option<Named> {
    let resolved = types.get(&candidate.c_name)?;
    // Every type recovered from the headers is an enumeration or a flag set, and both are
    // `Copy` on the Rust side, so a method takes one by value.
    let named = |arrays| Named {
        name: resolved.def.rust_name.clone(),
        arrays,
        by_value: true,
    };
    if candidate.signature == resolved.signature {
        Some(named(0))
    } else if candidate.signature == format!("a{}", resolved.signature) {
        Some(named(1))
    } else {
        None
    }
}

/// The Rust type each site of an interface is read as, where its signature undersells it.
///
/// The order of the fallbacks is the precedence: a hand-written wrapper — necessarily named
/// deliberately, in `annotations.toml` — wins over a type the headers merely name, which wins
/// over a typed object path, a dictionary shape, and finally a connection profile. Each is only
/// applied where the signature agrees with it, so a stale annotation leaves the member as its
/// structural type instead of one it cannot decode.
pub fn resolved_names(
    interface: &Interface<'_>,
    docs: &Docs,
    types: &BTreeMap<String, Resolved<'_>>,
    objects: &crate::objects::Targets,
    vardicts: &[crate::overrides::Vardict],
    newtypes: &[(&str, &str, &str)],
) -> BTreeMap<Site, Named> {
    let name = interface.name().to_string();
    let empty = BTreeMap::new();
    let object_sites = objects.get(name.as_str()).unwrap_or(&empty);
    let references: BTreeMap<Site, Named> = interface_candidates(interface, docs)
        .into_iter()
        .filter_map(|(site, candidate)| {
            reference_for(&candidate, types).map(|named| (site, named))
        })
        .collect();

    let mut out = BTreeMap::new();
    for property in interface.properties() {
        let member = property.name().to_string();
        let signature = property.ty().to_string();
        let site = Site::Property(member.clone());
        let named = named_newtype(newtypes, &name, &member, &signature)
            .or_else(|| references.get(&site).cloned())
            .or_else(|| named_object(object_sites, &site, &signature))
            .or_else(|| named_vardict(vardicts, &name, &member, &signature))
            .or_else(|| named_profile(&signature));
        if let Some(named) = named {
            out.insert(site, named);
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
    for (member, args) in members {
        for arg in args {
            let Some(arg_name) = arg.name() else { continue };
            let site = Site::Arg(member.clone(), arg_name.to_owned());
            let signature = arg.ty().inner().to_string();
            let named = references
                .get(&site)
                .cloned()
                .or_else(|| named_object(object_sites, &site, &signature))
                .or_else(|| {
                    named_vardict(vardicts, &name, &format!("{member}.{arg_name}"), &signature)
                })
                .or_else(|| named_profile(&signature));
            if let Some(named) = named {
                out.insert(site, named);
            }
        }
    }

    out
}

/// The Rust names members reach the trait under, where their own would not do.
///
/// Two things force a rename. NetworkManager gives some interfaces a `State` property *and* a
/// `StateChanged` signal, or a `Managed` property *and* a `SetManaged` method; both halves of
/// each pair want one Rust name, and the property keeps it, since its accessors are not
/// negotiable. And a member returning object paths moves aside so that the plain name can hand
/// back something more useful: `get_devices` gives proxies, `get_device_paths` the paths.
///
/// Keyed by the member name on the wire, e.g. `StateChanged` -> `state_changed_signal`.
pub type Renames = BTreeMap<String, String>;

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
    /// The object-path arguments it takes, which the accessor takes as objects.
    pub inputs: Vec<InputPath>,
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

/// An object-path argument of a method, as the accessor beside it takes it.
#[derive(Debug, Clone)]
pub struct InputPath {
    /// The parameter, spelled as [`crate::proxy::arg_name`] spells it, e.g. `device`.
    pub name: String,
    /// The interface at the other end, where the spec settles which it is. `None` is
    /// `specific_object`, which is an access point for Wi-Fi and an active connection for VPN,
    /// and so is taken as a bare path or as none at all.
    pub target: Option<String>,
    /// Whether the argument is a list of paths.
    pub list: bool,
}

/// A method with several outputs, as planned before its raw name is settled.
struct PlannedTuple {
    member: String,
    /// One per output: what to bind it to, and the interface it points at where it is a path.
    outputs: Vec<(String, Option<String>)>,
    description: String,
}

/// A method returning several things at once, at least one of them an object path.
///
/// `AddAndActivateConnection` hands back the profile it added and the activation it started;
/// both are paths, and both are more useful as the objects they name.
#[derive(Debug, Clone)]
pub struct TupleAccessor {
    /// The renamed raw member, e.g. `add_and_activate_connection_paths`.
    pub raw: String,
    /// The accessor to generate, e.g. `add_and_activate_connection`.
    pub wrapper: String,
    /// The object-path arguments it takes, which the accessor takes as objects.
    pub inputs: Vec<InputPath>,
    /// How the member is described, for the accessor's documentation.
    pub description: String,
    /// One per output, in order: what to bind it to, and the interface it points at where it
    /// is a path.
    pub outputs: Vec<(String, Option<String>)>,
}

/// A method that takes an object path and returns nothing that is one.
#[derive(Debug, Clone)]
pub struct InputAccessor {
    /// The renamed raw member, e.g. `deactivate_connection_path`.
    pub raw: String,
    /// The accessor to generate, e.g. `deactivate_connection`.
    pub wrapper: String,
    /// How the member is described, for the accessor's documentation.
    pub description: String,
    /// The object-path arguments it takes.
    pub inputs: Vec<InputPath>,
}

/// What a member's paths become, planned for one interface.
#[derive(Debug, Default, Clone)]
pub struct Accessors {
    /// Members returning one path, or a list of them.
    pub single: Vec<PathAccessor>,
    /// Methods returning a path among several outputs.
    pub tuples: Vec<TupleAccessor>,
    /// Methods that only take them.
    pub inputs: Vec<InputAccessor>,
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
) -> (Renames, Accessors) {
    let mut renames = plan_collisions(interface);
    let mut accessors = Accessors::default();

    // Read-only path properties, and methods whose single output is a path.
    let mut candidates: Vec<(Site, String, bool, String)> = Vec::new();
    // And methods with several outputs, one of which is a path.
    let mut tuples: Vec<PlannedTuple> = Vec::new();
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

    // What each method takes as an object path, which its accessor takes as an object.
    let mut method_inputs: BTreeMap<String, Vec<InputPath>> = BTreeMap::new();
    for method in interface.methods() {
        let member = method.name().to_string();
        let inputs: Vec<InputPath> = method
            .args()
            .iter()
            .filter(|a| a.direction() != Some(ArgDirection::Out))
            .filter_map(|arg| {
                let name = arg.name()?;
                let signature = arg.ty().inner().to_string();
                let list = match signature.as_str() {
                    crate::objects::PATH => false,
                    crate::objects::PATH_ARRAY => true,
                    _ => return None,
                };
                let site = Site::Arg(member.clone(), name.to_owned());
                Some(InputPath {
                    name: crate::proxy::arg_name(name),
                    target: objects.get(&site).cloned(),
                    list,
                })
            })
            .collect();
        if !inputs.is_empty() {
            method_inputs.insert(member, inputs);
        }
    }

    for method in interface.methods() {
        let outputs: Vec<_> = method
            .args()
            .iter()
            .filter(|a| a.direction() == Some(ArgDirection::Out))
            .collect();
        let member = method.name().to_string();
        match outputs.as_slice() {
            [output] => {
                let Some(arg_name) = output.name() else {
                    continue;
                };
                let signature = output.ty().inner().to_string();
                if matches!(signature.as_str(), "o" | "ao") {
                    candidates.push((
                        Site::Arg(member.clone(), arg_name.to_owned()),
                        member.clone(),
                        signature == "ao",
                        format!("the `{member}` method"),
                    ));
                }
            }
            // Several outputs, which are wrapped only if one of them is a path whose target is
            // known; the rest are handed back as they are.
            outputs => {
                let planned: Vec<(String, Option<String>)> = outputs
                    .iter()
                    .filter_map(|output| {
                        let name = output.name()?;
                        let site = Site::Arg(member.clone(), name.to_owned());
                        let target = (output.ty().inner().to_string() == crate::objects::PATH)
                            .then(|| objects.get(&site).cloned())
                            .flatten();
                        Some((snake(name), target))
                    })
                    .collect();
                if planned.len() == outputs.len()
                    && planned.iter().any(|(_, target)| target.is_some())
                {
                    tuples.push(PlannedTuple {
                        description: format!("the `{member}` method"),
                        member: member.clone(),
                        outputs: planned,
                    });
                }
            }
        }
    }

    for (site, member, list, description) in candidates {
        let Some(target) = objects.get(&site) else {
            continue;
        };
        // A member already renamed for a collision keeps that name; wrapping it as well would
        // need the two rules to agree, and none of them overlap in practice.
        if renames.contains_key(&member) {
            continue;
        }
        let wrapper = snake(&member);
        let raw = raw_path_name(&wrapper, list);
        renames.insert(member.clone(), raw.clone());
        let signal_arg = signals.contains(&member).then(|| match &site {
            Site::Arg(_, arg) => arg.clone(),
            Site::Property(name) => name.clone(),
        });
        accessors.single.push(PathAccessor {
            inputs: method_inputs.get(&member).cloned().unwrap_or_default(),
            raw,
            wrapper,
            target: target.clone(),
            list,
            property: matches!(site, Site::Property(_)),
            signal_arg,
            description,
        });
    }

    for PlannedTuple {
        member,
        outputs,
        description,
    } in tuples
    {
        if renames.contains_key(&member) {
            continue;
        }
        let wrapper = snake(&member);
        let paths = outputs
            .iter()
            .filter(|(_, target)| target.is_some())
            .count();
        let raw = raw_path_name(&wrapper, paths > 1);
        renames.insert(member.clone(), raw.clone());
        accessors.tuples.push(TupleAccessor {
            inputs: method_inputs.get(&member).cloned().unwrap_or_default(),
            raw,
            wrapper,
            description,
            outputs,
        });
    }

    // The methods that take a path and return nothing that is one. They are wrapped for their
    // arguments alone, so the raw member moves aside as it does for the rest.
    for (member, inputs) in &method_inputs {
        if renames.contains_key(member)
            || !inputs.iter().any(|input| input.target.is_some())
        {
            continue;
        }
        let wrapper = snake(member);
        let raw = raw_path_name(&wrapper, inputs.iter().any(|input| input.list));
        renames.insert(member.clone(), raw.clone());
        accessors.inputs.push(InputAccessor {
            raw,
            wrapper,
            description: format!("the `{member}` method"),
            inputs: inputs.clone(),
        });
    }

    (renames, accessors)
}

/// Rewrite a raw member's parameter list so that its object paths are taken as objects.
///
/// The parameters come from the signature the raw member was generated from, so the two cannot
/// describe different arguments; only the path ones are rewritten, and the rest are passed
/// straight through.
fn converted_parameters(
    sig: &MemberSig,
    inputs: &[InputPath],
    interfaces: &[String],
) -> (Vec<TokenStream>, Vec<TokenStream>) {
    let mut parameters = Vec::new();
    let mut forwarded = Vec::new();

    for (name, ty) in &sig.inputs {
        let ident = name.to_string();
        let input = inputs
            .iter()
            .find(|input| input.name == ident)
            .filter(|input| match &input.target {
                // An interface this run did not generate has no path type to take.
                Some(target) => interfaces.iter().any(|i| i == target),
                None => !input.list,
            });

        let Some(input) = input else {
            parameters.push(quote!(#name: #ty));
            forwarded.push(quote!(#name));
            continue;
        };

        match &input.target {
            Some(target) => {
                let path = format_ident!("{}", path_type_name(&crate::codegen::module_name(target)));
                match input.list {
                    true => {
                        parameters
                            .push(quote!(#name: impl IntoIterator<Item = impl Into<#path>>));
                        forwarded
                            .push(quote!(&#name.into_iter().map(Into::into).collect::<Vec<_>>()));
                    }
                    false => {
                        parameters.push(quote!(#name: impl Into<#path>));
                        forwarded.push(quote!(&#name.into()));
                    }
                }
            }
            // Which kind of object it is depends on the connection being activated, so it stays
            // a bare path — but `/` means none, and that is worth saying.
            None => {
                parameters.push(quote!(#name: Option<&zbus::zvariant::ObjectPath<'_>>));
                forwarded.push(quote!(#name.unwrap_or(&crate::path::NULL_OBJECT_PATH)));
            }
        }
    }

    (parameters, forwarded)
}

/// The members whose plain Rust name is already spoken for by a property's accessors.
fn plan_collisions(interface: &Interface<'_>) -> Renames {
    let mut taken: Vec<String> = Vec::new();
    for property in interface.properties() {
        let snake = snake(property.name().as_str());
        taken.push(format!("receive_{snake}_changed"));
        if property.access().write() {
            taken.push(format!("set_{snake}"));
        }
        taken.push(snake);
    }

    let mut renames = Renames::new();
    let mut claim = |wanted: String, member: &str, suffix: &'static str| {
        if !taken.contains(&wanted) {
            taken.push(wanted);
            return;
        }
        renames.insert(member.to_owned(), format!("{}_{suffix}", snake(member)));
    };

    for method in interface.methods() {
        let name = method.name().to_string();
        claim(snake(&name), &name, "method");
    }
    for signal in interface.signals() {
        let name = signal.name().to_string();
        claim(format!("receive_{}", snake(&name)), &name, "signal");
    }

    renames
}

/// The snake case member names reach Rust in.
fn snake(name: &str) -> String {
    snakecase::ascii::to_snakecase(name).into_owned()
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
            "    /// The values `{}` takes, sent over D-Bus as `s`.",
            set.describe()
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

/// The hand-written type a property is read through.
///
/// The entry only applies if the signature it claims is the one the property really carries; a
/// wrong one is dropped here and reported by [`crate::values::check_member_newtypes`], rather
/// than silently typing the member as something it cannot decode.
fn named_newtype(
    newtypes: &[(&str, &str, &str)],
    interface: &str,
    property: &str,
    signature: &str,
) -> Option<Named> {
    let (_, _, ty) = newtypes
        .iter()
        .find(|(i, member, _)| *i == interface && *member == property)?;
    let (_, wire) = crate::values::newtype_wire(ty)?;
    (wire == signature).then(|| Named {
        name: (*ty).to_owned(),
        arrays: 0,
        // Each wraps a string or a byte array, so a method takes one by reference.
        by_value: false,
    })
}

/// The dictionary shape a member carries, where one is written down for it.
fn named_vardict(
    vardicts: &[crate::overrides::Vardict],
    interface: &str,
    member: &str,
    signature: &str,
) -> Option<Named> {
    // A list of them where a property carries several, and one where a member takes exactly
    // one — the options dictionaries of the `*2` methods being the latter.
    let arrays = match signature {
        VARDICT_ARRAY_SIGNATURE => 1,
        VARDICT_SIGNATURE => 0,
        _ => return None,
    };
    let target = format!("{interface}.{member}");
    vardicts
        .iter()
        .find(|vardict| vardict.applies.iter().any(|a| a == &target))
        .map(|vardict| Named {
            name: vardict.name.clone(),
            arrays,
            by_value: false,
        })
}

/// The typed path a member carries, where the object at the other end is known.
fn named_object(sites: &BTreeMap<Site, String>, site: &Site, signature: &str) -> Option<Named> {
    let interface = sites.get(site)?;
    let arrays = match signature {
        crate::objects::PATH => 0,
        crate::objects::PATH_ARRAY => 1,
        _ => return None,
    };
    Some(Named {
        name: path_type_name(&crate::codegen::module_name(interface)),
        arrays,
        by_value: false,
    })
}

/// The connection profile a member carries, where its signature says it does.
fn named_profile(signature: &str) -> Option<Named> {
    (signature == PROFILE_SIGNATURE).then(|| Named {
        name: "ConnectionProfile".to_owned(),
        arrays: 0,
        by_value: false,
    })
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
        P::ValueArray { name } | P::NewtypeArray { name } => format!("Vec<{name}>"),
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
             }}\n\
             \n\
             /// So that a member taking one of these takes the proxy just as readily.\n\
             impl From<&crate::proxies::{module}::{trait_name}Proxy<'_>> for {path_name} {{\n    \
             fn from(proxy: &crate::proxies::{module}::{trait_name}Proxy<'_>) -> Self {{\n        \
             proxy.object_path()\n    \
             }}\n\
             }}\n",
            module = module.module,
            trait_name = module.trait_name,
        );
    }

    out
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
) -> TokenStream {
    // Each device type that has an interface, with the proxy and variant it becomes.
    struct Arm {
        /// The `DeviceType` variant, e.g. `Wifi`.
        device_type: syn::Ident,
        /// The `AnyDevice` variant, e.g. `Wireless`.
        variant: syn::Ident,
        proxy: syn::Type,
        interface: String,
    }
    let arms: Vec<Arm> = device_type
        .rust_variants()
        .into_iter()
        .filter_map(|(variant, variant_name)| {
            let interface = interfaces.get(&variant.nick)?;
            let module = modules.iter().find(|m| &m.interface == interface)?;
            let path = format_ident!("{}", module.module);
            let proxy = format_ident!("{}Proxy", module.trait_name);
            Some(Arm {
                device_type: format_ident!("{variant_name}"),
                variant: format_ident!("{}", crate::codegen::root_alias(modules, module, "")),
                proxy: parse_quote!(proxies::#path::#proxy<'static>),
                interface: interface.clone(),
            })
        })
        .collect();

    let header = [
        " Narrowing a device to what it actually is.".to_owned(),
        String::new(),
        format!(
            " Generated by `just generate` from the introspection data of NetworkManager \
             {nm_version}. Do not edit."
        ),
    ];
    let variants = arms.iter().map(|arm| {
        let doc = doc_line(&format!(
            "[`DeviceType::{}`], which implements `{}`.",
            arm.device_type, arm.interface
        ));
        let (variant, proxy) = (&arm.variant, &arm.proxy);
        quote!(#doc #variant(#proxy),)
    });
    let variant_names: Vec<&syn::Ident> = arms.iter().map(|arm| &arm.variant).collect();
    let device_types: Vec<&syn::Ident> = arms.iter().map(|arm| &arm.device_type).collect();

    quote! {
        #(#![doc = #header])*

        use crate::generated::proxies;
        use crate::generated::types::DeviceType;

        /// A device, narrowed to the interface its [`DeviceType`] implies.
        ///
        /// Produced by [`DeviceProxy::specialise`](crate::DeviceProxy::specialise), which
        /// reads the type once. A variant can only be built after its type has been seen, so
        /// unlike a cast there is no way to hold a wireless proxy for a wired device.
        #[derive(Debug, Clone)]
        #[non_exhaustive]
        pub enum AnyDevice {
            #(#variants)*
            /// A device whose type has no interface of its own — a loopback or a generic
            /// device — or one added to NetworkManager since this binding was generated.
            Other(DeviceType),
        }

        impl AnyDevice {
            /// The type NetworkManager reported, whichever variant this is.
            pub fn device_type(&self) -> DeviceType {
                match self {
                    #(Self::#variant_names(_) => DeviceType::#device_types,)*
                    Self::Other(ty) => *ty,
                }
            }
        }

        impl proxies::device::DeviceProxy<'_> {
            /// Narrow this device to the interface its type implies.
            ///
            /// ```no_run
            /// # use nm_automata::{AnyDevice, DeviceProxy};
            /// # async fn example(device: DeviceProxy<'_>) -> zbus::Result<()> {
            /// match device.specialise().await? {
            ///     AnyDevice::Wireless(wifi) => {
            ///         println!("{:?}", wifi.get_access_points().await?);
            ///     }
            ///     AnyDevice::Wired(wired) => {
            ///         println!("{}", wired.speed().await?);
            ///     }
            ///     other => println!("{:?}", other.device_type()),
            /// }
            /// # Ok(())
            /// # }
            /// ```
            pub async fn specialise(&self) -> zbus::Result<AnyDevice> {
                use crate::ProxyExt as _;
                let device_type = self.device_type().await?;
                Ok(match device_type {
                    #(DeviceType::#device_types => AnyDevice::#variant_names(self.cast().await?),)*
                    other => AnyDevice::Other(other),
                })
            }
        }
    }
}

/// Emit the accessors that turn a member's object paths into the proxies they point at.
///
/// These are what make `get_devices` hand back devices rather than paths. They live on the
/// proxy as inherent methods, using the connection the proxy already holds, so nothing has to
/// be passed in.
///
/// `members` is what [`crate::proxy`] just generated the trait from, so an accessor's
/// parameters and return type are the ones its raw member really has, not a second guess at
/// them.
pub fn path_accessors(
    members: &BTreeMap<String, MemberSig>,
    accessors: &Accessors,
    trait_name: &str,
    module: &str,
    interfaces: &[String],
) -> TokenStream {
    let proxy_trait = format_ident!("{trait_name}Proxy");
    let own_path = format_ident!("{}", path_type_name(module));
    let mut items = Vec::new();

    // Where this proxy is, in the form the members that take a path expect. `path` is not
    // available as a name: `Device.Path` is a real property, the one udev calls `ID_PATH`.
    items.push(quote! {
        /// The path of the object this proxy is for.
        ///
        /// Typed, so it can be handed to a member that takes one — activating a connection
        /// wants the device's path, and this is where to get it.
        pub fn object_path(&self) -> crate::paths::#own_path {
            crate::paths::#own_path::from(zbus::zvariant::OwnedObjectPath::from(
                self.inner().path().to_owned(),
            ))
        }
    });

    for accessor in &accessors.single {
        if !interfaces.iter().any(|i| i == &accessor.target) {
            continue;
        }
        let proxy = proxy_type(&accessor.target);
        let raw = format_ident!("{}", accessor.raw);
        let wrapper = format_ident!("{}", accessor.wrapper);

        // A signal becomes a stream of objects rather than an accessor.
        if let Some(arg) = &accessor.signal_arg {
            let arg = format_ident!("{}", crate::proxy::arg_name(arg));
            let receive_raw = format_ident!("receive_{}", accessor.raw);
            let receive_wrapper = format_ident!("receive_{}", accessor.wrapper);
            let doc = [
                doc_line(&format!(
                    "The objects {} carries, as they arrive.",
                    accessor.description
                )),
                doc_line(""),
                doc_line("Each is built on this proxy's own connection."),
                doc_line(&format!(
                    "[`receive_{raw}`](Self::receive_{raw}) gives the signal itself, carrying \
                     the path.",
                    raw = accessor.raw
                )),
            ];
            // The future is boxed so that the stream stays `Unpin`, and can be polled with
            // `next()` without being pinned first.
            items.push(quote! {
                #(#doc)*
                pub async fn #receive_wrapper(
                    &self,
                ) -> zbus::Result<impl futures_util::Stream<Item = zbus::Result<#proxy>> + use<>>
                {
                    let connection = self.inner().connection().clone();
                    let signals = self.#receive_raw().await?;
                    Ok(futures_util::StreamExt::then(signals, move |signal| {
                        let connection = connection.clone();
                        Box::pin(async move {
                            crate::NmPath::connect(signal.args()?.#arg(), &connection).await
                        })
                    }))
                }
            });
            continue;
        }

        let Some(sig) = members.get(&accessor.raw) else {
            continue;
        };
        let (parameters, forwarded) = converted_parameters(sig, &accessor.inputs, interfaces);
        let mut doc = vec![
            doc_line(&format!("The objects {} points at.", accessor.description)),
            doc_line(""),
            doc_line(&format!(
                "Built on this proxy's own connection. [`{raw}`](Self::{raw}) gives the paths \
                 themselves.",
                raw = accessor.raw
            )),
        ];

        // A property with nothing to point at points at `/`, where there is no object of any
        // kind — so the accessor says so, rather than handing back a proxy whose every call
        // would fail. A method returns an error instead of `/`, and a list leaves the object
        // out rather than listing it, so neither of those needs it.
        if accessor.property && !accessor.list {
            doc.extend([
                doc_line(""),
                doc_line("[`None`] if the property is [`/`](crate::NULL_PATH), which is how"),
                doc_line("NetworkManager says there is no such object."),
            ]);
            items.push(quote! {
                #(#doc)*
                pub async fn #wrapper(&self #(, #parameters)*) -> zbus::Result<Option<#proxy>> {
                    let Some(path) = self.#raw(#(#forwarded),*).await?.non_null() else {
                        return Ok(None);
                    };
                    crate::NmPath::connect(&path, self.inner().connection())
                        .await
                        .map(Some)
                }
            });
        } else if accessor.list {
            items.push(quote! {
                #(#doc)*
                pub async fn #wrapper(&self #(, #parameters)*) -> zbus::Result<Vec<#proxy>> {
                    let paths = self.#raw(#(#forwarded),*).await?;
                    crate::NmPathSliceExt::connect_all(&paths, self.inner().connection()).await
                }
            });
        } else {
            items.push(quote! {
                #(#doc)*
                pub async fn #wrapper(&self #(, #parameters)*) -> zbus::Result<#proxy> {
                    let path = self.#raw(#(#forwarded),*).await?;
                    crate::NmPath::connect(&path, self.inner().connection()).await
                }
            });
        }
    }

    for accessor in &accessors.tuples {
        let Some(sig) = members.get(&accessor.raw) else {
            continue;
        };
        if sig.outputs.len() != accessor.outputs.len() {
            continue;
        }
        let (parameters, forwarded) = converted_parameters(sig, &accessor.inputs, interfaces);

        // An output whose interface this run did not generate keeps its path.
        let wrapped: Vec<Option<syn::Type>> = accessor
            .outputs
            .iter()
            .map(|(_, target)| {
                target
                    .as_ref()
                    .filter(|target| interfaces.iter().any(|i| i == *target))
                    .map(|target| proxy_type(target))
            })
            .collect();
        if wrapped.iter().all(Option::is_none) {
            continue;
        }

        let returned: Vec<syn::Type> = wrapped
            .iter()
            .zip(&sig.outputs)
            .map(|(proxy, raw)| proxy.clone().unwrap_or_else(|| raw.clone()))
            .collect();
        let bindings: Vec<syn::Ident> = accessor
            .outputs
            .iter()
            .map(|(binding, _)| format_ident!("{binding}"))
            .collect();
        let values: Vec<TokenStream> = wrapped
            .iter()
            .zip(&bindings)
            .map(|(proxy, binding)| match proxy {
                Some(_) => {
                    quote!(crate::NmPath::connect(&#binding, self.inner().connection()).await?)
                }
                None => quote!(#binding),
            })
            .collect();

        let raw = format_ident!("{}", accessor.raw);
        let wrapper = format_ident!("{}", accessor.wrapper);
        let doc = [
            doc_line(&format!("The objects {} returns.", accessor.description)),
            doc_line(""),
            doc_line(&format!(
                "Built on this proxy's own connection. [`{raw}`](Self::{raw}) gives the paths \
                 themselves.",
                raw = accessor.raw
            )),
        ];
        items.push(quote! {
            #(#doc)*
            pub async fn #wrapper(&self #(, #parameters)*) -> zbus::Result<(#(#returned),*)> {
                let (#(#bindings),*) = self.#raw(#(#forwarded),*).await?;
                Ok((#(#values),*))
            }
        });
    }

    for accessor in &accessors.inputs {
        let Some(sig) = members.get(&accessor.raw) else {
            continue;
        };
        let (parameters, forwarded) = converted_parameters(sig, &accessor.inputs, interfaces);
        // Nothing was converted, so the accessor would only repeat the member it calls.
        if parameters.len() == forwarded.len()
            && sig
                .inputs
                .iter()
                .zip(&parameters)
                .all(|((name, ty), parameter)| {
                    quote!(#name: #ty).to_string() == parameter.to_string()
                })
        {
            continue;
        }

        let raw = format_ident!("{}", accessor.raw);
        let wrapper = format_ident!("{}", accessor.wrapper);
        let result = &sig.result;
        let doc = [
            doc_line(&format!(
                "{}, taking the objects rather than their paths.",
                upper_first(&accessor.description)
            )),
            doc_line(""),
            doc_line(&format!(
                "[`{raw}`](Self::{raw}) takes the paths themselves.",
                raw = accessor.raw
            )),
        ];
        items.push(quote! {
            #(#doc)*
            pub async fn #wrapper(&self #(, #parameters)*) -> zbus::Result<#result> {
                self.#raw(#(#forwarded),*).await
            }
        });
    }

    quote! {
        impl #proxy_trait<'_> {
            #(#items)*
        }
    }
}

/// The proxy an object at a path is reached through.
fn proxy_type(target: &str) -> syn::Type {
    let module = format_ident!("{}", crate::codegen::module_name(target));
    let name = format_ident!("{}Proxy", target.rsplit('.').next().unwrap_or(target));
    parse_quote!(crate::proxies::#module::#name<'static>)
}

/// `the `X` method` -> `The `X` method`, for a sentence that starts with one.
fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => text.to_owned(),
    }
}

/// Emit the module of dictionary shapes.
///
/// These are the members whose dictionary keys NetworkManager describes only in prose. Most are
/// read-only properties, where a key typed wrongly is a decode error rather than a corrupted
/// profile — which is what makes hand-written shapes acceptable at all. `ActivationOptions` is
/// the exception and is held to a higher standard: it is an argument this crate *sends*, so it
/// carries only the keys upstream spells out.
pub fn vardict_module(
    vardicts: &[crate::overrides::Vardict],
    types: &BTreeMap<String, Resolved<'_>>,
    values: &[crate::values::ValueSet],
    nm_version: &str,
) -> Result<String, anyhow::Error> {
    // The hand-written types any key is read through, which live at the crate root.
    let mut used: Vec<&str> = vardicts
        .iter()
        .flat_map(|vardict| &vardict.keys)
        .map(|key| key.ty.trim_start_matches("array of "))
        .filter(|element| crate::values::newtype_wire(element).is_some())
        .collect();
    used.sort_unstable();
    used.dedup();
    let newtypes = match used.is_empty() {
        true => String::new(),
        false => format!(
            "#[allow(unused_imports)]\nuse crate::{{{}}};\n",
            used.join(", ")
        ),
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! The dictionaries NetworkManager describes the keys of only in prose.\n\
         //!\n\
         //! Most are configuration it reports and this crate only reads. The `Options` and\n\
         //! `Args` ones are what it sends, and carry only the keys upstream spells out.\n\
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
         {newtypes}\
         use crate::settings::{{self, SettingMap, SettingsError}};\n"
    );

    for vardict in vardicts {
        let fields: Vec<(&crate::overrides::VardictKey, String, bool, String)> = vardict
            .keys
            .iter()
            .map(|key| {
                let ty = vardict_key_type(&key.ty, values).with_context(|| {
                    format!(
                        "the type of `{}.{}`, which is none of a wire type, an \
                         `[[enumeration]]` in annotations.toml, or one of nm-automata's own \
                         types",
                        vardict.name, key.name
                    )
                })?;
                let rendered = rust_type(&ty, types);
                let argument = setter_argument(&ty, &rendered);
                Ok((key, rendered, ty.is_copy(), argument))
            })
            .collect::<Result<_, anyhow::Error>>()?;

        let _ = writeln!(out, "/// {}", vardict.doc);
        let _ = writeln!(
            out,
            "///\n/// Keys not listed here are kept in [`other`](field@Self::other), since\n\
             /// NetworkManager documents most of these as carrying further ones."
        );
        let _ = writeln!(out, "#[derive(Debug, Default, Clone, PartialEq)]");
        let _ = writeln!(out, "pub struct {} {{", vardict.name);
        for (key, ty, _, _) in &fields {
            let _ = writeln!(out, "    /// {}", key.doc);
            let _ = writeln!(out, "    ///");
            let _ = writeln!(out, "    /// `{}`", key.name);
            let _ = writeln!(out, "    pub {}: Option<{ty}>,", field_ident(&key.name));
        }
        let _ = writeln!(
            out,
            "    /// Keys with no field of their own.\n    pub other: SettingMap,\n}}\n"
        );

        let idents: Vec<String> = fields
            .iter()
            .map(|(key, _, _, _)| field_ident(&key.name))
            .collect();
        let setters: Vec<(&str, &str, String)> = fields
            .iter()
            .zip(&idents)
            .map(|((key, _, _, argument), ident)| {
                (ident.as_str(), key.name.as_str(), argument.clone())
            })
            .collect();
        write_setters(&mut out, &vardict.name, "key", &setters);

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
        for (key, _, _, _) in &fields {
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
        for (key, _, is_copy, _) in &fields {
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

/// What a dictionary key travels as.
///
/// A wire type as the settings table spells it, or the name of a type this generation produces:
/// one of the `[[enumeration]]`s, or one of the hand-written types in `nm-automata`. `array of`
/// in front of either is a list of them, which is how `RequestScan` takes its SSIDs.
fn vardict_key_type(
    ty: &str,
    values: &[crate::values::ValueSet],
) -> Result<crate::settings::PropertyType, anyhow::Error> {
    use crate::settings::PropertyType as P;

    let (element, array) = match ty.strip_prefix("array of ") {
        Some(element) => (element, true),
        None => (ty, false),
    };
    let name = element.to_owned();
    if values.iter().any(|set| set.rust_name == element) {
        return Ok(match array {
            true => P::ValueArray { name },
            false => P::Value { name },
        });
    }
    if crate::values::newtype_wire(element).is_some() {
        return Ok(match array {
            true => P::NewtypeArray { name },
            false => P::Newtype { name },
        });
    }
    P::parse(ty)
}

/// `next-hop` -> `next_hop`.
fn field_ident(name: &str) -> String {
    name.replace('-', "_").to_ascii_lowercase()
}

/// The signature one of these dictionaries travels as.
pub const VARDICT_SIGNATURE: &str = "a{sv}";

/// And the signature of a list of them, which is how a property carries several.
pub const VARDICT_ARRAY_SIGNATURE: &str = "aa{sv}";

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
