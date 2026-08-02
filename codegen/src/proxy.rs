//! Emitting the `#[proxy]` trait for one D-Bus interface.
//!
//! This replaces `zbus_xmlgen`, which took introspection XML and gave back a string of Rust
//! source. Everything this crate adds beside a generated member — the accessors that turn
//! object paths into proxies — needs that member's parameter list and return type, and a
//! string is the one shape those cannot be read out of. Owning the emitter means both are
//! typed once, in [`MemberSig`], and handed straight to [`crate::render::path_accessors`].
//!
//! The signature-to-Rust mapping below is ported from `zbus_xmlgen`, which keeps it private.
//! Its awkward corners are upstream's, and the comments explaining them are kept with the code
//! they explain.
//!
//! One deliberate difference: every member states its D-Bus name in `#[zbus(name = "…")]`
//! rather than leaving it to be derived from the Rust identifier. The derivation lives in
//! `zbus_macros`, and matching it would mean keeping a copy of its case conversion in step
//! with a crate that has no reason to tell us when it changes. Saying the name outright costs
//! a line per member and cannot drift.

use std::collections::BTreeMap;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse_quote;
use zbus::zvariant::Signature;
use zbus_xml::{Arg, ArgDirection, Interface};

use crate::annotate::InterfaceDocs;
use crate::render::Site;

/// A Rust type a member is read as, where its D-Bus signature is not the whole story.
///
/// An enumeration-valued property is `u` on the wire; a path property is `o` but points at a
/// known interface. Both reach Rust as something better, named here.
#[derive(Debug, Clone)]
pub struct Named {
    /// The Rust type, e.g. `DeviceState`.
    pub name: String,
    /// How many array wrappers sit above it: `DeviceState[]` is one.
    pub arrays: usize,
    /// Whether the type is `Copy`, so that inputs take it by value rather than by reference.
    pub by_value: bool,
}

impl Named {
    fn base(&self) -> syn::Type {
        syn::parse_str(&self.name).expect("a resolved type name is a Rust type")
    }

    /// The form a member returns, or a signal carries: owned, however deeply nested.
    fn output(&self) -> syn::Type {
        (0..self.arrays).fold(self.base(), |ty, _| parse_quote!(Vec<#ty>))
    }

    /// The form a method takes, which is borrowed unless the type is cheap to copy.
    fn input(&self) -> syn::Type {
        let base = self.base();
        match self.arrays {
            0 if self.by_value => base,
            0 => parse_quote!(&#base),
            arrays => {
                let inner = (0..arrays - 1).fold(base, |ty, _| parse_quote!(Vec<#ty>));
                parse_quote!(&[#inner])
            }
        }
    }
}

/// The Rust types one generated member was built from.
///
/// This is what the accessors written beside a member are generated against, so that the two
/// cannot describe different arguments: there is one mapping from signature to type, run once.
#[derive(Debug, Clone)]
pub struct MemberSig {
    /// The parameters after `&self`, in order.
    pub inputs: Vec<(syn::Ident, syn::Type)>,
    /// The values the member yields, one per output argument.
    pub outputs: Vec<syn::Type>,
    /// What those values become inside the member's `zbus::Result`.
    pub result: syn::Type,
}

impl MemberSig {
    /// The result of a member with the given outputs.
    ///
    /// A lone structure output is wrapped in a one-element tuple, which is how zbus tells it
    /// apart from a method returning several values.
    fn new(inputs: Vec<(syn::Ident, syn::Type)>, outputs: Vec<syn::Type>, lone_struct: bool) -> Self {
        let result = match outputs.as_slice() {
            [] => parse_quote!(()),
            [ty] if lone_struct => parse_quote!((#ty,)),
            [ty] => ty.clone(),
            tys => parse_quote!((#(#tys),*)),
        };
        Self {
            inputs,
            outputs,
            result,
        }
    }

    /// The parameter list to repeat after `&self`.
    pub fn declaration(&self) -> Vec<TokenStream> {
        self.inputs
            .iter()
            .map(|(name, ty)| quote!(#name: #ty))
            .collect()
    }
}

/// Everything generated for one interface.
pub struct Emitted {
    /// The trait, with its `#[proxy]` attribute and doc comment.
    pub tokens: TokenStream,
    /// The signature of each member, by the Rust name it was generated under.
    pub members: BTreeMap<String, MemberSig>,
}

/// What an interface needs to become a proxy trait.
pub struct Proxy<'a> {
    pub interface: &'a Interface<'a>,
    pub docs: Option<&'a InterfaceDocs>,
    /// The Rust type to read a site as, where the signature alone would undersell it.
    pub named: &'a BTreeMap<Site, Named>,
    /// The Rust name to generate a member under, where its own would collide.
    pub renames: &'a BTreeMap<String, String>,
    pub service: &'a str,
    pub default_path: &'a str,
}

impl Proxy<'_> {
    pub fn emit(&self) -> Emitted {
        let mut members = BTreeMap::new();
        let mut items = Vec::new();

        for method in sorted(self.interface.methods(), |m| m.name().to_string()) {
            items.push(self.method(&method, &mut members));
        }
        for signal in sorted(self.interface.signals(), |s| s.name().to_string()) {
            items.push(self.signal(&signal, &mut members));
        }
        for property in sorted(self.interface.properties(), |p| p.name().to_string()) {
            items.push(self.property(&property, &mut members));
        }

        let name = self.interface.name();
        let trait_name = format_ident!("{}", trait_name(name.as_str()));
        let interface = name.as_str();
        let service = self.service;
        let path = self.default_path;
        let docs = doc_block(self.docs.and_then(|d| d.doc.as_deref()));

        let tokens = quote! {
            #(#docs)*
            #[proxy(
                interface = #interface,
                default_service = #service,
                default_path = #path
            )]
            pub trait #trait_name {
                #(#items)*
            }
        };

        Emitted { tokens, members }
    }

    /// The Rust name a member is generated under, and the attribute stating its wire name.
    fn naming(&self, member: &str) -> (syn::Ident, TokenStream) {
        let rust = match self.renames.get(member) {
            Some(renamed) => renamed.clone(),
            None => snake(member),
        };
        (identifier(&rust), quote!(name = #member))
    }

    fn method(
        &self,
        method: &zbus_xml::Method<'_>,
        members: &mut BTreeMap<String, MemberSig>,
    ) -> TokenStream {
        let member = method.name().to_string();
        let docs = self.docs.and_then(|d| d.members.get(&member));
        let (rust_name, wire_name) = self.naming(&member);

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut lone_struct = false;
        for arg in method.args() {
            let site = Site::Arg(member.clone(), arg.name().unwrap_or_default().to_owned());
            match arg.direction() {
                Some(ArgDirection::Out) => {
                    lone_struct = matches!(**arg.ty(), Signature::Structure(_));
                    outputs.push(match self.named.get(&site) {
                        Some(named) => named.output(),
                        None => signature_type(arg.ty(), false, false),
                    });
                }
                _ => {
                    let ty = match self.named.get(&site) {
                        Some(named) => named.input(),
                        None => signature_type(arg.ty(), true, true),
                    };
                    inputs.push((arg_ident(arg, inputs.len()), ty));
                }
            }
        }
        // Only a single output can be a bare structure; several are a tuple already.
        lone_struct &= outputs.len() == 1;

        let doc = member_doc(&member, "method", docs, method.args());
        let sig = MemberSig::new(inputs, outputs, lone_struct);
        let parameters = sig.declaration();
        let result = sig.result.clone();
        members.insert(rust_name.to_string(), sig);

        quote! {
            #(#doc)*
            #[zbus(#wire_name)]
            fn #rust_name(&self #(, #parameters)*) -> zbus::Result<#result>;
        }
    }

    fn signal(
        &self,
        signal: &zbus_xml::Signal<'_>,
        members: &mut BTreeMap<String, MemberSig>,
    ) -> TokenStream {
        let member = signal.name().to_string();
        let docs = self.docs.and_then(|d| d.members.get(&member));
        let (rust_name, wire_name) = self.naming(&member);

        let mut inputs = Vec::new();
        for arg in signal.args() {
            let site = Site::Arg(member.clone(), arg.name().unwrap_or_default().to_owned());
            // A signal's payload is deserialized, so a named type is carried in its owned form.
            let ty = match self.named.get(&site) {
                Some(named) => named.output(),
                None => signature_type(arg.ty(), true, false),
            };
            inputs.push((arg_ident(arg, inputs.len()), ty));
        }

        let doc = member_doc(&member, "signal", docs, signal.args());
        let sig = MemberSig::new(inputs, Vec::new(), false);
        let parameters = sig.declaration();
        members.insert(rust_name.to_string(), sig);

        quote! {
            #(#doc)*
            #[zbus(signal, #wire_name)]
            fn #rust_name(&self #(, #parameters)*) -> zbus::Result<()>;
        }
    }

    fn property(
        &self,
        property: &zbus_xml::Property<'_>,
        members: &mut BTreeMap<String, MemberSig>,
    ) -> TokenStream {
        let member = property.name().to_string();
        let docs = self.docs.and_then(|d| d.members.get(&member));
        let (rust_name, wire_name) = self.naming(&member);
        let site = Site::Property(member.clone());
        let named = self.named.get(&site);

        let doc = member_doc(&member, "property", docs, &[]);
        let attribute = quote!(#[zbus(property, #wire_name)]);
        let mut out = TokenStream::new();

        if property.access().read() {
            // A named type satisfies both directions of the property conversions, owned.
            let ty = match named {
                Some(named) => named.output(),
                None => signature_type(property.ty(), false, false),
            };
            members.insert(
                rust_name.to_string(),
                MemberSig::new(Vec::new(), vec![ty.clone()], false),
            );
            out.extend(quote! {
                #(#doc)*
                #attribute
                fn #rust_name(&self) -> zbus::Result<#ty>;
            });
        }

        if property.access().write() {
            let setter = format_ident!("set_{rust_name}");
            let ty = match named {
                Some(named) => named.output(),
                None => property_setter_type(property.ty()),
            };
            out.extend(quote! {
                #attribute
                fn #setter(&self, value: #ty) -> zbus::Result<()>;
            });
        }

        out
    }
}

/// The trait name an interface gets: its last dot-separated segment.
fn trait_name(interface: &str) -> &str {
    interface.rsplit('.').next().unwrap_or(interface)
}

fn sorted<T: Clone, K: Ord>(items: &[T], key: impl Fn(&T) -> K) -> Vec<T> {
    let mut items = items.to_vec();
    items.sort_by_key(key);
    items
}

/// The parameter name for an argument, or `arg_1` where the spec leaves it unnamed.
fn arg_ident(arg: &Arg, index: usize) -> syn::Ident {
    match arg.name() {
        Some(name) => identifier(&arg_name(name)),
        None => format_ident!("arg_{}", index + 1),
    }
}

/// The parameter name an argument reaches Rust under.
///
/// The accessors written beside a member match their own arguments against these, so both
/// sides have to agree on the spelling — hence one function rather than two rules.
pub fn arg_name(name: &str) -> String {
    name.replace('-', "_")
}

/// A member's doc comment: what it is, what it does, and what its arguments mean.
fn member_doc(
    member: &str,
    kind: &str,
    docs: Option<&crate::annotate::MemberDocs>,
    args: &[Arg],
) -> Vec<TokenStream> {
    let mut out = vec![doc_line(&format!("{member} {kind}"))];
    if let Some(body) = docs.and_then(|d| d.doc.as_deref()) {
        out.push(doc_line(""));
        out.extend(paragraphs(body));
    }

    let documented: Vec<(&str, &str)> = args
        .iter()
        .filter_map(|arg| {
            let name = arg.name()?;
            let doc = docs?.args.get(name)?.doc.as_deref()?;
            Some((name, doc))
        })
        .collect();
    if !documented.is_empty() {
        out.push(doc_line(""));
        out.push(doc_line("# Arguments"));
        out.push(doc_line(""));
        for (name, doc) in documented {
            let text = collapse(doc);
            out.push(doc_line(&format!("* `{}` - {text}", name.replace('-', "_"))));
        }
    }
    out
}

/// The doc comment for the trait itself.
fn doc_block(doc: Option<&str>) -> Vec<TokenStream> {
    doc.map(paragraphs).unwrap_or_default()
}

/// Documentation as rustdoc paragraphs: each collapsed to a line, separated by blank ones.
fn paragraphs(text: &str) -> Vec<TokenStream> {
    let mut out = Vec::new();
    for paragraph in text.split("\n\n") {
        let line = collapse(paragraph);
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(doc_line(""));
        }
        out.push(doc_line(&line));
    }
    out
}

/// Squeeze a paragraph onto one line, which is how rustdoc reads it anyway.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One line of a doc comment, as the attribute `///` is shorthand for.
///
/// `quote!` accepts `///` directly, but nothing can be interpolated into one — so every line
/// carrying a generated name has to be built as an attribute instead.
pub fn doc_line(text: &str) -> TokenStream {
    let text = match text.is_empty() {
        true => String::new(),
        false => format!(" {text}"),
    };
    quote!(#[doc = #text])
}

fn snake(name: &str) -> String {
    snakecase::ascii::to_snakecase(name).into_owned()
}

/// The Rust keywords, which a D-Bus name is free to collide with.
const KEYWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "union", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

fn identifier(name: &str) -> syn::Ident {
    match KEYWORDS.contains(&name) {
        true => format_ident!("{}_", name),
        false => format_ident!("{}", name),
    }
}

/// The Rust type a D-Bus signature travels as.
///
/// `input` selects the form a method takes rather than returns, and `as_ref` the borrowed one;
/// the two differ because a signal's payload is owned even though it is an input.
fn signature_type(signature: &Signature, input: bool, as_ref: bool) -> syn::Type {
    match signature {
        Signature::Unit => parse_quote!(()),
        Signature::U8 => parse_quote!(u8),
        Signature::Bool => parse_quote!(bool),
        Signature::I16 => parse_quote!(i16),
        Signature::U16 => parse_quote!(u16),
        Signature::I32 => parse_quote!(i32),
        Signature::U32 => parse_quote!(u32),
        Signature::I64 => parse_quote!(i64),
        Signature::U64 => parse_quote!(u64),
        Signature::F64 => parse_quote!(f64),
        Signature::Fd if input => parse_quote!(zbus::zvariant::Fd<'_>),
        Signature::Fd => parse_quote!(zbus::zvariant::OwnedFd),
        Signature::Str if input || as_ref => parse_quote!(&str),
        Signature::Str => parse_quote!(String),
        Signature::ObjectPath if input && as_ref => {
            parse_quote!(&zbus::zvariant::ObjectPath<'_>)
        }
        Signature::ObjectPath if input => parse_quote!(zbus::zvariant::ObjectPath<'_>),
        Signature::ObjectPath => parse_quote!(zbus::zvariant::OwnedObjectPath),
        // `zvariant::Signature` has been lifetime-less, with no owned counterpart, since
        // zvariant 5.
        Signature::Signature if input && as_ref => parse_quote!(&zbus::zvariant::Signature),
        Signature::Signature => parse_quote!(zbus::zvariant::Signature),
        Signature::Variant if input && as_ref => parse_quote!(&zbus::zvariant::Value<'_>),
        Signature::Variant if input => parse_quote!(zbus::zvariant::Value<'_>),
        Signature::Variant => parse_quote!(zbus::zvariant::OwnedValue),
        Signature::Array(child) => {
            let child = signature_type(child, input, as_ref);
            match input && as_ref {
                true => parse_quote!(&[#child]),
                false => parse_quote!(Vec<#child>),
            }
        }
        Signature::Dict { key, value } => {
            let key = signature_type(key, input, as_ref);
            let value = signature_type(value, input, as_ref);
            parse_quote!(std::collections::HashMap<#key, #value>)
        }
        Signature::Structure(fields) => {
            let fields: Vec<syn::Type> = fields
                .iter()
                .map(|field| signature_type(field, input, as_ref))
                .collect();
            let tuple: syn::Type = match fields.as_slice() {
                [field] => parse_quote!((#field,)),
                fields => parse_quote!((#(#fields),*)),
            };
            match as_ref {
                true => parse_quote!(&#tuple),
                false => tuple,
            }
        }
        // `Maybe` is gvariant-only, and never appears in D-Bus introspection.
        #[allow(unreachable_patterns)]
        other => unreachable!("unsupported signature: {other}"),
    }
}

/// Like [`signature_type`] for an input, but tailored to property setters.
///
/// `Proxy::set_property` requires the value to satisfy `T: Into<Value<'t>>`. For most types the
/// `&T` form already does, via `From<&T> for Value`. For a variant or a structure it does not,
/// so the owned form is used instead — including where either is nested inside an array or a
/// dictionary.
fn property_setter_type(signature: &Signature) -> syn::Type {
    match signature {
        Signature::Variant => parse_quote!(zbus::zvariant::Value<'_>),
        Signature::Structure(fields) => {
            let fields: Vec<syn::Type> = fields.iter().map(property_setter_type).collect();
            match fields.as_slice() {
                [field] => parse_quote!((#field,)),
                fields => parse_quote!((#(#fields),*)),
            }
        }
        Signature::Array(child) => {
            let child = property_setter_type(child);
            parse_quote!(&[#child])
        }
        Signature::Dict { key, value } => {
            let key = property_setter_type(key);
            let value = property_setter_type(value);
            parse_quote!(std::collections::HashMap<#key, #value>)
        }
        other => signature_type(other, true, true),
    }
}
