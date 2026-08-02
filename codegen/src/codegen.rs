//! Turning the vendored spec into `nm-automata/src/generated/`.
//!
//! The flow is: read the spec, recover the annotations NetworkManager keeps in comments, and
//! emit Rust from both. [`crate::proxy`] writes the `#[proxy]` traits and [`crate::render`]
//! everything around them — the enumerations, flag sets, typed paths, dictionary shapes and
//! connection profiles the traits exchange.
//!
//! Every file is emitted as a token stream and printed with `prettyplease`, which means a
//! module that does not parse is caught here rather than by the crate that has to compile it.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use zbus_xml::Node;

use crate::annotate::{self, Docs};
use crate::fetch;
use crate::gir::{self, TypeDef};
use crate::objects;
use crate::overrides;
use crate::proxy;
use crate::render::{self, Resolved};
use crate::settings;
use crate::values;

const SERVICE: &str = "org.freedesktop.NetworkManager";

/// Produce every generated file, keyed by the path it belongs at.
pub fn generate(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let nm_version = fetch::vendored_version(root)?;
    let spec = root.join("spec");

    let gir_path = spec.join("NM-1.0.gir");
    let gir_source = std::fs::read_to_string(&gir_path).with_context(|| {
        format!(
            "reading {} — run `just build-spec` first",
            gir_path.display()
        )
    })?;
    let defs = gir::parse(&gir_source)?;
    let known: HashSet<String> = defs.iter().map(|d| d.c_name.clone()).collect();

    let annotations = overrides::load(root)?;

    let settings_path = spec.join("nm-settings-docs-dbus.xml");
    let settings_source = std::fs::read_to_string(&settings_path).with_context(|| {
        format!(
            "reading {} — run `just build-spec` first",
            settings_path.display()
        )
    })?;
    let mut profile_settings = settings::parse(&settings_source, &annotations.setting_fields())?;

    // The properties that are worth more than the table's `string` or `byte array`: those whose
    // values NetworkManager enumerates, and those the crate has a type for.
    let constants = gir::parse_constants(&gir_source)?;
    let value_sets = values::resolve(
        values::derive(&constants, &profile_settings),
        annotations.enumerations(),
        &profile_settings,
    )?;
    values::apply(&mut profile_settings, &value_sets)?;
    values::apply_newtypes(&mut profile_settings, annotations.newtypes())?;
    let profile_settings = profile_settings;

    let sources = read_dir(&spec.join("introspection"), "xml")?;
    let parsed: Vec<(String, Node<'_>, Docs)> = sources
        .iter()
        .map(|(name, xml)| {
            let node = Node::try_from(xml.as_str())
                .with_context(|| format!("parsing spec/introspection/{name}"))?;
            let mut docs = annotate::extract(xml, &known)?;
            annotations.apply(&mut docs, &known)?;
            Ok((name.clone(), node, docs))
        })
        .collect::<Result<_>>()?;

    // What kind of object each object path points at, needed before any interface is written.
    let interface_names: Vec<String> = parsed
        .iter()
        .flat_map(|(_, node, _)| node.interfaces().iter().map(|i| i.name().to_string()))
        .collect();
    let object_targets = objects::resolve(&parsed, &interface_names, &annotations.object_targets());

    // The interface members read through one of the crate's own types, checked here so that an
    // entry which no longer matches the spec fails rather than silently ceasing to apply.
    let member_newtypes = annotations.member_newtypes();
    let properties: Vec<(String, String, String)> = parsed
        .iter()
        .flat_map(|(_, node, _)| node.interfaces())
        .flat_map(|interface| {
            interface.properties().iter().map(|property| {
                (
                    interface.name().to_string(),
                    property.name().to_string(),
                    property.ty().to_string(),
                )
            })
        })
        .collect();
    values::check_member_newtypes(annotations.newtypes(), &properties)?;

    // And the members that carry one of the hand-written dictionary shapes, likewise: a
    // `[[vardict]]` naming something the spec does not have would otherwise apply to nothing.
    let dictionaries: Vec<(String, String)> = parsed
        .iter()
        .flat_map(|(_, node, _)| render::dictionary_sites(node))
        .collect();
    annotations.check_vardicts(&dictionaries)?;

    let types = resolve_types(&defs, &parsed);
    let by_c_name: BTreeMap<String, Resolved<'_>> = types
        .iter()
        .map(|r| (r.def.c_name.clone(), r.clone()))
        .collect();

    let mut modules: Vec<Generated> = Vec::new();
    // Which annotations actually landed, so that a stale entry in `annotations.toml` is an
    // error rather than a silent no-op.
    let mut applied: HashSet<(String, String)> = HashSet::new();

    for (_, node, docs) in &parsed {
        for interface in node.interfaces() {
            for (site, _) in render::interface_candidates(interface, docs) {
                let member = match site {
                    render::Site::Property(property) => property,
                    render::Site::Arg(member, arg) => format!("{member}.{arg}"),
                };
                applied.insert((interface.name().to_string(), member));
            }
        }

        for interface in node.interfaces() {
            let iface_name = interface.name().to_string();
            let empty = BTreeMap::new();
            let sites = object_targets.get(&iface_name).unwrap_or(&empty);
            let (renames, accessors) = render::plan(interface, sites);

            let named = render::resolved_names(
                interface,
                docs,
                &by_c_name,
                &object_targets,
                annotations.vardicts(),
                &member_newtypes,
            );
            let emitted = proxy::Proxy {
                interface,
                docs: docs.interfaces.get(&iface_name),
                named: &named,
                renames: &renames,
                service: SERVICE,
                default_path: &default_path(&iface_name),
            }
            .emit();

            let accessors = render::path_accessors(
                &emitted.members,
                &accessors,
                trait_name(&iface_name),
                &module_name(&iface_name),
                &interface_names,
            );
            let code = emitted.tokens;
            modules.push(Generated {
                module: module_name(&iface_name),
                trait_name: trait_name(&iface_name).to_owned(),
                interface: iface_name,
                code: quote! { #code #accessors },
            });
        }
    }
    modules.sort_by(|a, b| a.module.cmp(&b.module));

    annotations.check_all_applied(&applied)?;
    check_name_clashes(&types, &value_sets, annotations.vardicts(), &modules)?;

    let generated = root.join("nm-automata/src/generated");
    let mut files = BTreeMap::new();
    // The hand-written types live at the crate root rather than in a generated module, so the
    // proxies that use one have to name it.
    let mut wrappers: Vec<&str> = member_newtypes.iter().map(|(_, _, ty)| *ty).collect();
    wrappers.sort_unstable();
    wrappers.dedup();

    for module in &modules {
        files.insert(
            generated.join(format!("proxies/{}.rs", module.module)),
            print_tokens(proxy_module(module, &wrappers, &nm_version), &module.module)?,
        );
    }
    files.insert(
        generated.join("proxies/mod.rs"),
        print(&proxies_mod(&modules, &nm_version), "proxies/mod.rs")?,
    );
    files.insert(
        generated.join("types.rs"),
        print(
            &render::types_module(&types, &value_sets, &nm_version),
            "types.rs",
        )?,
    );
    files.insert(
        generated.join("vardict.rs"),
        print(
            &render::vardict_module(annotations.vardicts(), &by_c_name, &value_sets, &nm_version)?,
            "vardict.rs",
        )?,
    );
    files.insert(generated.join("devices.rs"), {
        let device_type = defs
            .iter()
            .find(|d| d.c_name == "NMDeviceType")
            .context("the introspection data has no NMDeviceType")?;
        let interfaces = device_interfaces(&defs, &modules, &annotations.device_interfaces())?;
        print(
            &render::devices_module(device_type, &interfaces, &modules, &nm_version),
            "devices.rs",
        )?
    });
    files.insert(
        generated.join("paths.rs"),
        print(
            &render::paths_module(&object_targets, &modules, &nm_version),
            "paths.rs",
        )?,
    );
    files.insert(
        generated.join("settings.rs"),
        print(
            &render::settings_module(&profile_settings, &by_c_name, &nm_version),
            "settings.rs",
        )?,
    );
    files.insert(
        generated.join("mod.rs"),
        print(&mod_module(&nm_version), "mod.rs")?,
    );

    Ok(files)
}

/// One generated proxy module.
pub struct Generated {
    /// The module it lives in, e.g. `device_wireless`.
    pub module: String,
    /// The D-Bus interface it proxies.
    pub interface: String,
    /// The trait name derived from the interface, e.g. `Wireless`.
    pub trait_name: String,
    /// The trait itself, and the accessors written beside it.
    code: TokenStream,
}

/// Read every file in `dir` with the given extension, sorted by name so that generation is
/// reproducible.
fn read_dir(dir: &Path, extension: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading {} — run `just fetch` first", dir.display()))?
    {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == extension) {
            let name = path
                .file_name()
                .expect("a file with an extension has a name")
                .to_string_lossy()
                .into_owned();
            out.push((name, std::fs::read_to_string(&path)?));
        }
    }
    out.sort();
    Ok(out)
}

/// Decide, for every type, which integer it travels as.
///
/// The introspection data says nothing about wire types, so the answer comes from the sites that use each
/// type. Where those disagree the most common one wins and the odd site keeps a plain integer;
/// where a type is never referenced it falls back to `u`, which is what NetworkManager uses
/// everywhere it does say.
fn resolve_types<'a>(
    defs: &'a [TypeDef],
    parsed: &[(String, Node<'_>, Docs)],
) -> Vec<Resolved<'a>> {
    let mut counts: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for (_, node, docs) in parsed {
        for interface in node.interfaces() {
            for (_, candidate) in render::interface_candidates(interface, docs) {
                let signature = candidate.element_signature();
                if render::repr_for(signature).is_none() {
                    continue;
                }
                if let Some(def) = defs.iter().find(|d| d.c_name == candidate.c_name) {
                    *counts
                        .entry(def.c_name.as_str())
                        .or_default()
                        .entry(signature.to_owned())
                        .or_default() += 1;
                }
            }
        }
    }

    defs.iter()
        .map(|def| {
            let signature = counts
                .get(def.c_name.as_str())
                .and_then(|by_signature| {
                    by_signature
                        .iter()
                        .max_by_key(|(signature, count)| {
                            (*count, std::cmp::Reverse((*signature).clone()))
                        })
                        .map(|(signature, _)| signature.clone())
                })
                .unwrap_or_else(|| {
                    // Nothing in the spec names this type, so fall back to what NetworkManager
                    // uses everywhere it does — widened to a signed integer for the handful of
                    // types with a negative sentinel value, such as `NM_TERNARY_DEFAULT`.
                    if def.variants.iter().any(|variant| variant.value < 0) {
                        "i".to_owned()
                    } else {
                        "u".to_owned()
                    }
                });
            let repr = render::repr_for(&signature).expect("only integer signatures are counted");
            Resolved {
                def,
                signature,
                repr,
            }
        })
        .collect()
}

/// D-Bus objects that expose an interface exactly once live at the path its name spells out:
/// `org.freedesktop.NetworkManager.Settings` at `/org/freedesktop/NetworkManager/Settings`.
///
/// Interfaces with many instances — devices, access points — get the same treatment, and the
/// resulting default is simply never useful; those proxies are built from a path obtained at
/// runtime instead.
fn default_path(interface: &str) -> String {
    format!("/{}", interface.replace('.', "/"))
}

/// Which interface each device type implies, by the type's introspection name.
///
/// Most are named after each other — a `bond` device implements `Device.Bond` — so only the
/// four that are not need `annotations.toml`: NetworkManager calls the type `ethernet` and the
/// interface `Wired`, `wifi` and `Wireless`, `bt` and `Bluetooth`, `6lowpan` and `Lowpan`.
fn device_interfaces(
    defs: &[TypeDef],
    modules: &[Generated],
    overrides: &[(String, String)],
) -> Result<BTreeMap<String, String>> {
    let device_type = defs
        .iter()
        .find(|d| d.c_name == "NMDeviceType")
        .context("the introspection data has no NMDeviceType")?;

    let candidates: BTreeMap<String, &str> = modules
        .iter()
        .filter_map(|module| {
            let tail = module
                .interface
                .strip_prefix("org.freedesktop.NetworkManager.Device.")?;
            Some((normalise(tail), module.interface.as_str()))
        })
        .collect();

    let mut out = BTreeMap::new();
    for variant in &device_type.variants {
        if let Some((_, interface)) = overrides.iter().find(|(ty, _)| ty == &variant.nick) {
            out.insert(variant.nick.clone(), interface.clone());
            continue;
        }
        if let Some(interface) = candidates.get(&normalise(&variant.nick)) {
            out.insert(variant.nick.clone(), (*interface).to_owned());
        }
    }

    for (ty, interface) in overrides {
        if !modules.iter().any(|m| &m.interface == interface) {
            bail!(
                "annotations.toml maps the `{ty}` device type to `{interface}`, which the spec does not have"
            );
        }
    }
    Ok(out)
}

fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The module name for an interface: `…NetworkManager.Device.Wireless` -> `device_wireless`.
pub fn module_name(interface: &str) -> String {
    let tail = interface
        .strip_prefix(SERVICE)
        .unwrap_or(interface)
        .trim_start_matches('.');
    if tail.is_empty() {
        return "network_manager".to_owned();
    }
    tail.split('.').map(to_snake).collect::<Vec<_>>().join("_")
}

/// The trait name an interface gets: its last dot-separated segment.
fn trait_name(interface: &str) -> &str {
    interface.rsplit('.').next().unwrap_or(interface)
}

fn proxy_module(module: &Generated, wrappers: &[&str], nm_version: &str) -> TokenStream {
    let wrappers: Vec<syn::Ident> = wrappers.iter().map(|ty| format_ident!("{ty}")).collect();
    let wrappers = match wrappers.is_empty() {
        true => TokenStream::new(),
        false => quote! {
            #[allow(unused_imports)]
            use crate::{#(#wrappers),*};
        },
    };

    let header = [
        format!(" Proxy for `{}`.", module.interface),
        String::new(),
        format!(
            " Generated by `just generate` from the introspection XML of NetworkManager \
             {nm_version}. Do not edit."
        ),
    ];
    let code = &module.code;

    quote! {
        #(#![doc = #header])*

        #![allow(
            clippy::too_many_arguments,
            clippy::type_complexity,
            rustdoc::invalid_html_tags
        )]

        #[allow(unused_imports)]
        use crate::generated::paths::*;
        #[allow(unused_imports)]
        use crate::generated::vardict::*;
        #[allow(unused_imports)]
        use crate::generated::settings::ConnectionProfile;
        #[allow(unused_imports)]
        use crate::generated::types::*;
        #wrappers
        use zbus::proxy;

        #code
    }
}

/// The module that ties the per-interface proxies together and lifts their names to the crate
/// root.
///
/// One module per interface is what keeps the names apart: `Settings.Connection` and
/// `VPN.Connection` both generate a `ConnectionProxy`, and three interfaces each generate a
/// `StateChanged` signal type. Names that are unique across the spec are re-exported as they
/// are; the rest get their module's name in front, which is how NetworkManager's own
/// documentation distinguishes them.
/// The name a module's proxy is re-exported under at the crate root.
///
/// Unique trait names keep their own; the rest take their module's, which is how
/// NetworkManager's documentation tells `Settings.Connection` and `VPN.Connection` apart.
pub fn root_alias(modules: &[Generated], module: &Generated, suffix: &str) -> String {
    let unique = modules
        .iter()
        .filter(|other| other.trait_name == module.trait_name)
        .count()
        == 1;
    if unique {
        format!("{}{suffix}", module.trait_name)
    } else {
        format!("{}{suffix}", render::pascal_case(&module.module))
    }
}

fn proxies_mod(modules: &[Generated], nm_version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Proxies for every NetworkManager D-Bus interface, one module each.\n\
         //!\n\
         //! Generated by `just generate` from the introspection XML of NetworkManager\n\
         //! {nm_version}. Do not edit.\n\
         //!\n\
         //! The `default_path` of each proxy is the one its interface name spells out. That is\n\
         //! the real path for the four singletons — `org.freedesktop.NetworkManager` itself and\n\
         //! its `Settings`, `AgentManager` and `DnsManager` — so `new(&connection)` reaches\n\
         //! them directly. Every other object is found at runtime; build proxies for those with\n\
         //! [`ObjectPathExt::proxy`](crate::ObjectPathExt::proxy) or\n\
         //! [`ProxyExt::cast`](crate::ProxyExt::cast).\n"
    );

    for module in modules {
        let _ = writeln!(out, "pub mod {};", module.module);
    }
    out.push('\n');

    for module in modules {
        let alias = |suffix: &str| root_alias(modules, module, suffix);
        let _ = writeln!(
            out,
            "#[doc(inline)]\npub use {module}::{trait_name}Proxy as {name};",
            module = module.module,
            trait_name = module.trait_name,
            name = alias("Proxy"),
        );
        let _ = writeln!(
            out,
            "#[cfg(feature = \"blocking-api\")]\n#[doc(inline)]\n\
             pub use {module}::{trait_name}ProxyBlocking as {name};",
            module = module.module,
            trait_name = module.trait_name,
            name = alias("ProxyBlocking"),
        );
    }

    out
}

fn mod_module(nm_version: &str) -> String {
    format!(
        "//! Code generated from the NetworkManager D-Bus spec. Do not edit.\n\
         //!\n\
         //! Generated by `just generate` from NetworkManager {nm_version}.\n\
         \n\
         pub mod devices;\n\
         pub mod paths;\n\
         pub mod proxies;\n\
         pub mod settings;\n\
         pub mod vardict;\n\
         pub mod types;\n\
         \n\
         /// The NetworkManager release this binding was generated from.\n\
         pub const NM_VERSION: &str = \"{nm_version}\";\n"
    )
}

/// Undo the XML escaping that carried documentation through the introspection file.
///
/// Only doc comments are touched; the surrounding code never contains an entity.
pub fn unescape_doc_comments(code: &str) -> String {
    code.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                line.replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fail loudly rather than emit code that will not compile, or that quietly shadows a name.
fn check_name_clashes(
    types: &[Resolved<'_>],
    values: &[values::ValueSet],
    vardicts: &[overrides::Vardict],
    modules: &[Generated],
) -> Result<()> {
    let mut taken: HashSet<String> = HashSet::new();
    for module in modules {
        taken.insert(format!("{}Proxy", module.trait_name));
        taken.insert(format!("{}ProxyBlocking", module.trait_name));
        taken.insert(render::pascal_case(&module.module) + "Proxy");
    }

    let mut seen: HashSet<&str> = HashSet::new();
    for resolved in types {
        let name = resolved.def.rust_name.as_str();
        if taken.contains(name) {
            bail!("NetworkManager type `{name}` collides with a generated proxy of the same name");
        }
        if !seen.insert(name) {
            bail!("two NetworkManager types both generate `{name}`");
        }
        // The enumeration macro adds an `Other` variant for values from newer daemons.
        if !resolved.def.is_flags()
            && let Some((variant, _)) = resolved
                .def
                .rust_variants()
                .into_iter()
                .find(|(_, name)| name == "Other")
        {
            bail!(
                "`{}` has a variant named `{}` that collides with the generated `Other`",
                resolved.def.c_name,
                variant.c_name
            );
        }
    }

    // The value sets share the namespace: they are generated into the same module and
    // re-exported at the crate root alongside everything else.
    for set in values {
        let name = set.rust_name.as_str();
        if taken.contains(name) {
            bail!(
                "the values of `{}` generate `{name}`, which collides with a generated proxy of \
                 the same name",
                set.describe()
            );
        }
        if !seen.insert(name) {
            bail!(
                "the values of `{}` generate `{name}`, which is already the name of a \
                 NetworkManager type",
                set.describe()
            );
        }
    }

    // The dictionary shapes are in the same namespace again, being re-exported alongside them.
    for vardict in vardicts {
        let name = vardict.name.as_str();
        if taken.contains(name) {
            bail!("the `{name}` dictionary shape collides with a generated proxy of the same name");
        }
        if !seen.insert(name) {
            bail!("the `{name}` dictionary shape is already the name of a generated type");
        }
    }
    Ok(())
}

fn to_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);

    for (i, &ch) in chars.iter().enumerate() {
        if i > 0 && ch.is_ascii_uppercase() {
            let previous = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(|c| c.is_ascii_lowercase());
            // A word boundary is a lower-to-upper step, the last capital of an acronym before
            // a new word, or a digit followed by a new word — but not the tail of `P2P`.
            if previous.is_ascii_lowercase()
                || ((previous.is_ascii_uppercase() || previous.is_ascii_digit()) && next_is_lower)
            {
                out.push('_');
            }
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Print a generated module, having checked that it is Rust at all.
///
/// `rustfmt` used to do this, as a separate process that might not be installed and whose
/// failure had to be shrugged off. `prettyplease` prints a parsed syntax tree instead, so a
/// module that does not parse fails here — naming the file — rather than in the crate that has
/// to compile it.
fn print_tokens(tokens: TokenStream, what: &str) -> Result<String> {
    let file = syn::parse2(tokens)
        .with_context(|| format!("the generated {what} is not valid Rust"))?;
    Ok(space_items(&prettyplease::unparse(&file)))
}

/// The same, for the modules still emitted as text.
///
/// A module whose items are macro invocations — `types.rs` and `paths.rs`, which are lists of
/// `nm_enum!`/`nm_flags!` and of path newtypes — is written as it was emitted. `prettyplease`
/// only knows the grammar of what it can parse, and a macro body is an opaque token stream to
/// it: printing one flattens the invocation onto ragged lines and turns every `///` inside it
/// into `#[doc = "…"]`. Those emitters lay their own output out, so there is nothing to gain by
/// running it through. It is still parsed here, which is what catches a module that is not Rust.
fn print(code: &str, what: &str) -> Result<String> {
    let file =
        syn::parse_file(code).with_context(|| format!("the generated {what} is not valid Rust"))?;
    if file.items.iter().any(|item| matches!(item, syn::Item::Macro(_))) {
        return Ok(format!("{}\n", code.trim_end()));
    }
    Ok(space_items(&prettyplease::unparse(&file)))
}

/// Put a blank line back between documented items.
///
/// `prettyplease` emits none at all, which reads as a wall in a file of a hundred enumerations.
/// Blank lines are whitespace, so the worst this can do is look odd: it inserts one before a
/// doc comment that follows something other than an attribute, another doc comment, or the
/// opening of a block.
fn space_items(code: &str) -> String {
    let mut out = String::with_capacity(code.len() + code.len() / 16);
    let mut previous = "";
    for line in code.lines() {
        let trimmed = line.trim_start();
        let starts_item = trimmed.starts_with("///");
        let continues = previous.is_empty()
            || previous.starts_with("///")
            || previous.starts_with("//!")
            || previous.starts_with("#[")
            || previous.ends_with('{');
        if starts_item && !continues {
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
        previous = trimmed;
    }
    out
}
