//! Turning the vendored spec into `netman/src/generated/`.
//!
//! The flow is: read the spec, recover the annotations NetworkManager keeps in comments, write
//! them back as Telepathy XML, and hand that to `zbus_xmlgen`. The proxies come out of the
//! generator; the enumerations and flag sets are emitted here, because `zbus_xmlgen` writes a
//! copy of every referenced type into every file that mentions it and has no notion of bit
//! sets at all.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use zbus::names::BusName;
use zbus::zvariant::ObjectPath;
use zbus_xml::Node;

use crate::annotate::{self, Docs};
use crate::fetch;
use crate::header::{self, TypeDef};
use crate::overrides;
use crate::render::{self, Resolved};

const SERVICE: &str = "org.freedesktop.NetworkManager";

/// Produce every generated file, keyed by the path it belongs at.
pub fn generate(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let nm_version = fetch::vendored_version(root)?;
    let spec = root.join("spec");

    let mut defs = Vec::new();
    for (name, source) in read_dir(&spec.join("headers"), "h")? {
        defs.extend(header::parse(&source).with_context(|| format!("parsing {name}"))?);
    }
    let known: HashSet<String> = defs.iter().map(|d| d.c_name.clone()).collect();

    let annotations = overrides::load(root)?;
    let sources = read_dir(&spec.join("introspection"), "xml")?;
    let parsed: Vec<(String, Node<'_>, Docs)> = sources
        .iter()
        .map(|(name, xml)| {
            let node = Node::try_from(xml.as_str())
                .with_context(|| format!("parsing spec/introspection/{name}"))?;
            let mut docs = annotate::extract(xml, &known);
            annotations.apply(&mut docs, &known)?;
            Ok((name.clone(), node, docs))
        })
        .collect::<Result<_>>()?;

    let types = resolve_types(&defs, &parsed);
    let by_c_name: BTreeMap<String, Resolved<'_>> = types
        .iter()
        .map(|r| (r.def.c_name.clone(), r.clone()))
        .collect();

    // Keeping the augmented XML around makes it possible to see exactly what the generator
    // was fed when a binding looks wrong.
    let debug_dir = root.join("target/netman-augmented");
    std::fs::create_dir_all(&debug_dir).ok();

    let mut modules: Vec<Generated> = Vec::new();
    // Which annotations actually landed, so that a stale entry in `annotations.toml` is an
    // error rather than a silent no-op.
    let mut applied: HashSet<(String, String)> = HashSet::new();

    for (name, node, docs) in &parsed {
        let augmented = render::augmented_xml(node, docs, &by_c_name);
        std::fs::write(debug_dir.join(name), &augmented).ok();

        let augmented_node = Node::try_from(augmented.as_str())
            .with_context(|| format!("re-reading the augmented {name}"))?;

        for interface in node.interfaces() {
            for (site, _) in render::interface_candidates(interface, docs) {
                let member = match site {
                    render::Site::Property(property) => property,
                    render::Site::Arg(member, arg) => format!("{member}.{arg}"),
                };
                applied.insert((interface.name().to_string(), member));
            }
        }

        for interface in augmented_node.interfaces() {
            let iface_name = interface.name().to_string();
            // The renames were planned against the original interface, before its colliding
            // members were written out under sentinel names.
            let renames = node
                .interfaces()
                .iter()
                .find(|original| original.name() == interface.name())
                .map(render::plan_renames)
                .unwrap_or_default();

            let code = interface_code(interface, augmented_node.telepathy_types())
                .with_context(|| format!("generating a proxy for {iface_name}"))?;
            let code = apply_renames(&code, &renames);
            modules.push(Generated {
                module: module_name(&iface_name),
                trait_name: trait_name(&iface_name).to_owned(),
                interface: iface_name,
                code,
            });
        }
    }
    modules.sort_by(|a, b| a.module.cmp(&b.module));

    annotations.check_all_applied(&applied)?;
    check_name_clashes(&types, &modules)?;

    let generated = root.join("netman/src/generated");
    let mut files = BTreeMap::new();
    for module in &modules {
        files.insert(
            generated.join(format!("proxies/{}.rs", module.module)),
            rustfmt(&proxy_module(module, &nm_version))?,
        );
    }
    files.insert(
        generated.join("proxies/mod.rs"),
        rustfmt(&proxies_mod(&modules, &nm_version))?,
    );
    files.insert(
        generated.join("types.rs"),
        rustfmt(&render::types_module(&types, &nm_version))?,
    );
    files.insert(generated.join("mod.rs"), rustfmt(&mod_module(&nm_version))?);

    Ok(files)
}

/// One generated proxy module.
struct Generated {
    /// The module it lives in, e.g. `device_wireless`.
    module: String,
    /// The D-Bus interface it proxies.
    interface: String,
    /// The trait name `zbus_xmlgen` derives from the interface, e.g. `Wireless`.
    trait_name: String,
    code: String,
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

/// Decide, for every type in the header, which integer it travels as.
///
/// The header says nothing about wire types, so the answer comes from the sites that use each
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

/// Generate the proxy for one interface, without the copy of the shared type definitions that
/// `zbus_xmlgen` prepends — those live in `types.rs`, emitted once.
fn interface_code(
    interface: &zbus_xml::Interface<'_>,
    node_types: &[zbus_xml::telepathy::TypeDef],
) -> Result<String> {
    let service = BusName::try_from(SERVICE)?;
    let path = default_path(interface.name().as_str());
    let path = ObjectPath::try_from(path.as_str())?;

    let code = zbus_xmlgen::CodeGenerator::new()
        .with_service(Some(&service))
        .with_path(Some(&path))
        .with_node_types(node_types)
        .with_format(false)
        .interface_code(interface)?;

    Ok(strip_type_defs(&code).to_owned())
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

/// Put the members generated under a sentinel name back under their real one.
///
/// The sentinel — `netman_rename_0` — was chosen so that `zbus_xmlgen` always writes the
/// D-Bus name out as an attribute (its snake and Pascal forms differ), and so that it appears
/// nowhere else in the generated source. Restoring the attribute and then replacing what is
/// left renames the Rust items without touching the wire.
fn apply_renames(code: &str, renames: &[render::Rename]) -> String {
    let mut code = code.to_owned();
    for rename in renames {
        code = code
            // The doc comment `zbus_xmlgen` writes above the member names it by hand.
            .replace(
                &format!("/// {} {}", rename.sentinel, rename.kind),
                &format!("/// {} {}", rename.member, rename.kind),
            )
            // The D-Bus name, which must go back to what NetworkManager actually calls it.
            .replace(
                &format!("name = \"{}\"", rename.sentinel),
                &format!("name = \"{}\"", rename.member),
            )
            // Everything remaining is a Rust identifier derived from the sentinel.
            .replace(&rename.sentinel_type, &rename.type_name)
            .replace(&rename.sentinel, &rename.fn_name);
    }
    code
}

/// Cut everything before the proxy trait: `zbus_xmlgen` writes the type definitions an
/// interface references ahead of it, and would repeat them for every interface.
fn strip_type_defs(code: &str) -> &str {
    let Some(offset) = code.find("#[proxy(") else {
        return code;
    };
    // The trait's own doc comment sits directly above the attribute.
    let mut start = offset;
    for line in code[..offset].lines().rev() {
        if !line.trim_start().starts_with("///") {
            break;
        }
        start -= line.len() + 1;
    }
    &code[start..]
}

/// The module name for an interface: `…NetworkManager.Device.Wireless` -> `device_wireless`.
fn module_name(interface: &str) -> String {
    let tail = interface
        .strip_prefix(SERVICE)
        .unwrap_or(interface)
        .trim_start_matches('.');
    if tail.is_empty() {
        return "network_manager".to_owned();
    }
    tail.split('.').map(to_snake).collect::<Vec<_>>().join("_")
}

/// The trait name `zbus_xmlgen` gives an interface: its last dot-separated segment.
fn trait_name(interface: &str) -> &str {
    interface.rsplit('.').next().unwrap_or(interface)
}

fn proxy_module(module: &Generated, nm_version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Proxy for `{}`.\n\
         //!\n\
         //! Generated by `just generate` from the introspection XML of NetworkManager\n\
         //! {nm_version}. Do not edit.\n\
         \n\
         #![allow(\n    clippy::too_many_arguments,\n    clippy::type_complexity,\n    \
         rustdoc::invalid_html_tags\n)]\n\n\
         #[allow(unused_imports)]\n\
         use crate::generated::types::*;\n\
         use zbus::proxy;\n",
        module.interface
    );
    out.push_str(&module.code);
    unescape_doc_comments(&out)
}

/// The module that ties the per-interface proxies together and lifts their names to the crate
/// root.
///
/// One module per interface is what keeps the names apart: `Settings.Connection` and
/// `VPN.Connection` both generate a `ConnectionProxy`, and three interfaces each generate a
/// `StateChanged` signal type. Names that are unique across the spec are re-exported as they
/// are; the rest get their module's name in front, which is how NetworkManager's own
/// documentation distinguishes them.
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

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for module in modules {
        *counts.entry(module.trait_name.as_str()).or_default() += 1;
    }

    for module in modules {
        let unique = counts[module.trait_name.as_str()] == 1;
        let alias = |suffix: &str| {
            if unique {
                format!("{}{suffix}", module.trait_name)
            } else {
                format!("{}{suffix}", render::pascal_case(&module.module))
            }
        };
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
         pub mod proxies;\n\
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
fn check_name_clashes(types: &[Resolved<'_>], modules: &[Generated]) -> Result<()> {
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
            && let Some(variant) = resolved
                .def
                .variants
                .iter()
                .find(|v| render::pascal_case(&v.suffix) == "Other")
        {
            bail!(
                "`{}` has a variant named `{}` that collides with the generated `Other`",
                resolved.def.c_name,
                variant.c_name
            );
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

fn rustfmt(code: &str) -> Result<String> {
    let mut child = match Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        // Formatting is a nicety; a missing rustfmt should not block regeneration.
        Err(_) => return Ok(code.to_owned()),
    };

    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().expect("stdin was piped");
        stdin.write_all(code.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Ok(code.to_owned());
    }
    Ok(String::from_utf8(output.stdout)?)
}
