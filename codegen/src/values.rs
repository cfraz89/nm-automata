//! The profile properties that are worth more than the type the settings table gives them.
//!
//! Two kinds, and both end up rewriting a [`Property`](crate::settings::Property)'s type:
//!
//! * a `string` property whose values are a fixed set — `802-11-wireless.mode` is one of seven —
//!   becomes an enumeration of them rather than a `String`;
//! * a `byte array` property that is not really bytes — an SSID, a hardware address — becomes
//!   the hand-written type that knows how to read them.
//!
//! # Where the values come from
//!
//! Mostly from the spec. NetworkManager declares each settings property as a string constant
//! (`NM_SETTING_WIRELESS_MODE`, `"mode"`), and where the property's values are also fixed
//! strings it declares those the same way, as constants extending the property's own name
//! (`NM_SETTING_WIRELESS_MODE_INFRA`, `"infrastructure"`). [`derive`] reads them back out of
//! that, which is why `mode` needs no annotation.
//!
//! It is careful about it, because the names alone are ambiguous:
//! `NM_SETTING_WIRELESS_MAC_ADDRESS_RANDOMIZATION` extends `NM_SETTING_WIRELESS_MAC_ADDRESS`
//! in exactly the same way without being a value of it. Three things rule that out, and each
//! comes from the spec rather than from a guess:
//!
//! * a constant is anchored to a setting by the `NM_SETTING_<X>_SETTING_NAME` constant that
//!   gives that setting's name, so `NM_SETTING_BLUETOOTH_TYPE` cannot be read as belonging to
//!   `connection.type`;
//! * the property it extends must be one the settings table types as `string`, and the value
//!   must be a string constant too — which is what excludes the integer limits and defaults;
//! * a constant whose value is itself a property of that setting is a property, not a value,
//!   which is what excludes `mac-address-randomization` and its like.
//!
//! Where NetworkManager does not declare the values — `802-11-wireless.band` has them in its
//! prose and nowhere else — `annotations.toml` supplies them, checked against the table the
//! same way.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use crate::gir::Constant;
use crate::overrides::{Enumeration, Newtype};
use crate::settings::{Property, PropertyType, Setting};

/// An enumeration of the strings one property takes.
#[derive(Debug, Clone)]
pub struct ValueSet {
    /// The type to generate, e.g. `WirelessMode`.
    pub rust_name: String,
    /// The setting the property belongs to, e.g. `802-11-wireless`; absent for a set that
    /// stands on its own, named by `annotations.toml` for a dictionary key to use.
    pub setting: Option<String>,
    /// The property itself, e.g. `mode`.
    pub property: Option<String>,
    pub doc: Option<String>,
    pub values: Vec<Value>,
}

impl ValueSet {
    /// How the set reads in a message: the property it belongs to, or its own name.
    pub fn describe(&self) -> String {
        match (&self.setting, &self.property) {
            (Some(setting), Some(property)) => format!("{setting}.{property}"),
            _ => self.rust_name.clone(),
        }
    }
}

/// One value of a [`ValueSet`].
#[derive(Debug, Clone)]
pub struct Value {
    /// The variant to generate, e.g. `Infrastructure`.
    pub rust_name: String,
    /// The string that travels, e.g. `infrastructure`.
    pub text: String,
    /// The constant NetworkManager declares it as, where it declares one.
    pub c_name: Option<String>,
    pub doc: Option<String>,
}

/// The constant that gives a setting its name, e.g. `SETTING_WIRELESS_SETTING_NAME`.
const NAME_SUFFIX: &str = "_SETTING_NAME";

/// Read the value sets NetworkManager's own constants describe.
///
/// See the module documentation for what makes a constant a value rather than a property.
pub fn derive(constants: &[Constant], settings: &[Setting]) -> Vec<ValueSet> {
    let by_name: BTreeMap<&str, &Setting> = settings.iter().map(|s| (s.name.as_str(), s)).collect();

    // Every setting, by the prefix its constants share: `SETTING_WIRELESS` -> `802-11-wireless`.
    let prefixes: Vec<(&str, &Setting)> = constants
        .iter()
        .filter_map(|constant| {
            let prefix = constant.name.strip_suffix(NAME_SUFFIX)?;
            Some((prefix, *by_name.get(constant.value.as_str())?))
        })
        .collect();

    let mut sets = Vec::new();
    for (prefix, setting) in prefixes {
        let of_setting: Vec<&Constant> = constants
            .iter()
            .filter(|constant| constant.name.starts_with(&format!("{prefix}_")))
            .collect();

        // What every property of this setting would be called as a constant. A constant with one
        // of these names names the property itself, whether or not NetworkManager declares it
        // under this prefix — `method` belongs to the shared `NMSettingIPConfig`, so it is
        // `NM_SETTING_IP_CONFIG_METHOD` while its values are `NM_SETTING_IP4_CONFIG_METHOD_*`.
        let named: Vec<(String, &Property)> = setting
            .properties
            .iter()
            .map(|property| (format!("{prefix}_{}", screaming(&property.name)), property))
            .collect();

        // The properties whose values could be enumerated at all.
        let properties: Vec<&(String, &Property)> = named
            .iter()
            .filter(|(_, property)| {
                matches!(property.ty, PropertyType::Str | PropertyType::StrArray)
            })
            .collect();

        for (name, property) in &properties {
            let base = format!("{name}_");
            let values: Vec<Value> = of_setting
                .iter()
                // A constant that names a property is a property, not a value: this is what
                // tells `NM_SETTING_WIRELESS_MAC_ADDRESS_RANDOMIZATION` from a value of
                // `mac-address`, while leaving `..._METHOD_LINK_LOCAL` a value of `method`
                // even though `link-local` is also a property of `ipv4`.
                .filter(|candidate| !named.iter().any(|(other, _)| *other == candidate.name))
                // The longest match wins, so that `SETTING_TEAM_RUNNER_HWADDR_POLICY_BY_ACTIVE`
                // is a value of `runner-hwaddr-policy` and not one of `runner`.
                .filter(|candidate| {
                    candidate.name.starts_with(&base)
                        && properties.iter().all(|(other, _)| {
                            other.len() <= name.len()
                                || !candidate.name.starts_with(&format!("{other}_"))
                        })
                })
                .map(|candidate| Value {
                    rust_name: variant_name(&candidate.value).unwrap_or_else(|| {
                        crate::render::pascal_case(&candidate.name[base.len()..])
                    }),
                    text: candidate.value.clone(),
                    c_name: Some(format!("NM_{}", candidate.name)),
                    doc: candidate.doc.clone(),
                })
                .collect();

            if values.is_empty() {
                continue;
            }
            sets.push(ValueSet {
                rust_name: type_name(setting, property),
                setting: Some(setting.name.clone()),
                property: Some(property.name.clone()),
                doc: None,
                values,
            });
        }
    }

    sets.sort_by(|a, b| {
        (&a.setting, &a.property, &a.rust_name).cmp(&(&b.setting, &b.property, &b.rust_name))
    });
    sets
}

/// Add the hand-written value sets to the derived ones, checking both against the table.
pub fn resolve(
    derived: Vec<ValueSet>,
    annotated: &[Enumeration],
    settings: &[Setting],
) -> Result<Vec<ValueSet>> {
    let mut sets = derived;

    for entry in annotated {
        // Either the values of a profile property, checked against the table, or a type of its
        // own for a dictionary key — which has nothing in the table to check it against, and is
        // checked instead by the key that names it having to resolve.
        let named = match entry.profile_property() {
            Some((setting_name, property_name)) => {
                let property = find(settings, setting_name, property_name)?;
                if !matches!(property.ty, PropertyType::Str | PropertyType::StrArray) {
                    bail!(
                        "annotations.toml enumerates the values of `{}`, which the settings \
                         table does not type as a string",
                        entry.describe()
                    );
                }
                if let Some(existing) = sets.iter().find(|set| {
                    set.setting.as_deref() == Some(setting_name)
                        && set.property.as_deref() == Some(property_name)
                }) {
                    bail!(
                        "annotations.toml enumerates the values of `{}`, which NetworkManager \
                         already declares constants for — the entry would only repeat {}",
                        entry.describe(),
                        existing.rust_name
                    );
                }
                let setting = settings
                    .iter()
                    .find(|s| s.name == setting_name)
                    .expect("`find` returned a property of it");
                (
                    type_name(setting, property),
                    Some(setting_name.to_owned()),
                    Some(property_name.to_owned()),
                )
            }
            None => (
                entry
                    .type_name()
                    .expect("an entry names one form or the other")
                    .to_owned(),
                None,
                None,
            ),
        };

        let values = entry
            .values
            .iter()
            .map(|value| {
                let rust_name = match (&value.name, variant_name(&value.value)) {
                    (Some(name), _) => name.clone(),
                    (None, Some(derived)) => derived,
                    (None, None) => bail!(
                        "annotations.toml gives `{}` the value `{}`, which cannot open a Rust \
                         identifier — it needs a `name` of its own",
                        entry.describe(),
                        value.value
                    ),
                };
                Ok(Value {
                    rust_name,
                    text: value.value.clone(),
                    c_name: None,
                    doc: value.doc.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let (rust_name, setting, property) = named;
        sets.push(ValueSet {
            rust_name,
            setting,
            property,
            doc: entry.doc.clone(),
            values,
        });
    }

    for set in &sets {
        check(set)?;
    }
    sets.sort_by(|a, b| {
        (&a.setting, &a.property, &a.rust_name).cmp(&(&b.setting, &b.property, &b.rust_name))
    });
    Ok(sets)
}

/// The hand-written types in `nm-automata` a member can be read through.
///
/// Each is listed with what it reads: the settings table's name for it, and the D-Bus signature
/// of the same thing, since these apply to interface members as well as to profile properties.
/// A type not listed here is rejected rather than generated against, so `annotations.toml`
/// cannot name something the crate does not have.
pub fn newtype_wire(name: &str) -> Option<(PropertyType, &'static str)> {
    Some(match name {
        "Ssid" | "MacAddress" => (PropertyType::ByteArray, "ay"),
        "AssignedMacAddress" => (PropertyType::Str, "s"),
        _ => return None,
    })
}

/// Give every annotated profile property the hand-written type it names.
///
/// The interface members among the entries are applied by [`crate::render`] instead, through the
/// same `tp:type` mechanism the object paths and dictionary shapes use.
pub fn apply_newtypes(settings: &mut [Setting], newtypes: &[Newtype]) -> Result<()> {
    for entry in newtypes {
        let Some((setting, property_name)) = entry.profile_property() else {
            continue;
        };
        let (wanted, _) = newtype_wire(&entry.ty).ok_or_else(|| unknown_newtype(entry))?;
        let property = find_mut(settings, setting, property_name)?;
        if property.ty != wanted {
            bail!(
                "annotations.toml gives `{}` the type `{}`, but the settings table types the \
                 property as {:?} rather than {wanted:?}",
                entry.describe(),
                entry.ty,
                property.ty,
            );
        }
        property.ty = PropertyType::Newtype {
            name: entry.ty.clone(),
        };
    }
    Ok(())
}

/// Check the entries that name an interface member against the introspection data.
///
/// The member has to exist and to travel as what the type reads. Without this an entry that no
/// longer matches would simply stop applying: `zbus_xmlgen` ignores a `tp:type` whose signature
/// disagrees with the member's, which is what keeps a wrong one from generating code that
/// cannot decode.
pub fn check_member_newtypes(
    newtypes: &[Newtype],
    interfaces: &[(String, String, String)],
) -> Result<()> {
    for entry in newtypes {
        let Some((interface, member)) = entry.interface_member() else {
            continue;
        };
        let (_, signature) = newtype_wire(&entry.ty).ok_or_else(|| unknown_newtype(entry))?;
        let found = interfaces
            .iter()
            .find(|(i, m, _)| i == interface && m == member)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "annotations.toml gives `{}` the type `{}`, but the vendored spec has no \
                     such property",
                    entry.describe(),
                    entry.ty
                )
            })?;
        if found.2 != signature {
            bail!(
                "annotations.toml gives `{}` the type `{}`, which reads `{signature}`, but the \
                 property travels as `{}`",
                entry.describe(),
                entry.ty,
                found.2
            );
        }
    }
    Ok(())
}

fn unknown_newtype(entry: &Newtype) -> anyhow::Error {
    anyhow::anyhow!(
        "annotations.toml gives `{}` the type `{}`, which nm-automata does not have — a new one \
         needs a type in the crate and a line in `values::newtype_wire`",
        entry.describe(),
        entry.ty
    )
}

/// Give every enumerated property the type generated for its values.
pub fn apply(settings: &mut [Setting], sets: &[ValueSet]) -> Result<()> {
    for set in sets {
        // The standalone ones belong to no property, so there is nothing to retype.
        let (Some(setting), Some(name)) = (&set.setting, &set.property) else {
            continue;
        };
        let property = find_mut(settings, setting, name)?;
        property.ty = match property.ty {
            PropertyType::StrArray => PropertyType::ValueArray {
                name: set.rust_name.clone(),
            },
            _ => PropertyType::Value {
                name: set.rust_name.clone(),
            },
        };
    }
    Ok(())
}

/// The type a property's values become: the setting it belongs to, then the property.
fn type_name(setting: &Setting, property: &Property) -> String {
    format!(
        "{}{}",
        crate::render::pascal_case(&setting.field_name),
        crate::render::pascal_case(&property.field_name)
    )
}

/// `mac-address` -> `MAC_ADDRESS`: a property name as NetworkManager spells it in a constant.
fn screaming(property: &str) -> String {
    property.replace('-', "_").to_ascii_uppercase()
}

/// The variant a value becomes, or `None` if it cannot open a Rust identifier.
fn variant_name(value: &str) -> Option<String> {
    let name = crate::render::pascal_case(&value.replace(['-', '.', ' '], "_"));
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        .then_some(name)
}

/// Fail on a set that would not generate, rather than emitting code that cannot compile.
fn check(set: &ValueSet) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for value in &set.values {
        // The macro adds this variant for values from newer daemons.
        if value.rust_name == "Other" {
            bail!(
                "`{}` has the value `{}`, which collides with the generated `Other`",
                set.describe(),
                value.text
            );
        }
        if seen.contains(&value.rust_name.as_str()) {
            bail!(
                "`{}` has two values that both generate `{}`",
                set.describe(),
                value.rust_name
            );
        }
        seen.push(&value.rust_name);
    }
    Ok(())
}

fn find<'a>(settings: &'a [Setting], setting: &str, property: &str) -> Result<&'a Property> {
    settings
        .iter()
        .find(|s| s.name == setting)
        .and_then(|s| s.properties.iter().find(|p| p.name == property))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "annotations.toml names `{setting}.{property}`, which the vendored settings \
                 table does not have"
            )
        })
}

fn find_mut<'a>(
    settings: &'a mut [Setting],
    setting: &str,
    property: &str,
) -> Result<&'a mut Property> {
    settings
        .iter_mut()
        .find(|s| s.name == setting)
        .and_then(|s| s.properties.iter_mut().find(|p| p.name == property))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "annotations.toml names `{setting}.{property}`, which the vendored settings \
                 table does not have"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(name: &str, value: &str) -> Constant {
        Constant {
            name: name.to_owned(),
            value: value.to_owned(),
            doc: None,
        }
    }

    const TABLE: &str = r#"<nm-setting-docs>
      <setting name="802-11-wireless" description="Wi-Fi" name_upper="WIRELESS">
        <property name="mode" type="string"/>
        <property name="band" type="string"/>
        <property name="mac-address" type="byte array"/>
        <property name="mac-address-randomization" type="uint32"/>
        <property name="ssid" type="byte array"/>
      </setting>
      <setting name="connection" description="General" name_upper="CONNECTION">
        <property name="type" type="string"/>
      </setting>
    </nm-setting-docs>"#;

    fn settings() -> Vec<Setting> {
        crate::settings::parse(TABLE, &[]).unwrap()
    }

    fn constants() -> Vec<Constant> {
        vec![
            constant("SETTING_WIRELESS_SETTING_NAME", "802-11-wireless"),
            constant("SETTING_WIRELESS_MODE", "mode"),
            constant("SETTING_WIRELESS_MODE_INFRA", "infrastructure"),
            constant("SETTING_WIRELESS_MODE_ADHOC", "adhoc"),
            constant("SETTING_WIRELESS_MAC_ADDRESS", "mac-address"),
            constant(
                "SETTING_WIRELESS_MAC_ADDRESS_RANDOMIZATION",
                "mac-address-randomization",
            ),
        ]
    }

    #[test]
    fn reads_the_values_a_property_declares() {
        let sets = derive(&constants(), &settings());
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].rust_name, "WirelessMode");
        assert_eq!(sets[0].property.as_deref(), Some("mode"));
        let values: Vec<_> = sets[0]
            .values
            .iter()
            .map(|v| (v.rust_name.as_str(), v.text.as_str()))
            .collect();
        // In the order the spec declares them, which is the order they read best in.
        assert_eq!(
            values,
            [("Infrastructure", "infrastructure"), ("Adhoc", "adhoc")]
        );
    }

    #[test]
    fn does_not_read_a_property_as_a_value_of_another() {
        // `mac-address-randomization` extends `mac-address` and is not one of its values; nor
        // is `mac-address` a candidate at all, being a byte array.
        let sets = derive(&constants(), &settings());
        assert!(
            sets.iter()
                .all(|set| set.property.as_deref() != Some("mac-address"))
        );
    }

    #[test]
    fn anchors_a_constant_to_the_setting_it_names() {
        // `SETTING_WIRELESS_MODE` must not be read as a value set for `connection.type`, or
        // for any other setting with a similarly-named property.
        let sets = derive(&constants(), &settings());
        assert!(
            sets.iter()
                .all(|set| set.setting.as_deref() == Some("802-11-wireless"))
        );
    }

    #[test]
    fn reads_values_of_a_property_declared_by_a_shared_base_class() {
        // `method` belongs to `NMSettingIPConfig`, which both address families derive from, so
        // the constant naming the property is `NM_SETTING_IP_CONFIG_METHOD` while its values
        // are `NM_SETTING_IP4_CONFIG_METHOD_*`. Anchoring on what the property *would* be
        // called under this setting's prefix is what finds them.
        let table = r#"<nm-setting-docs>
          <setting name="ipv4" description="IPv4" name_upper="IP4_CONFIG">
            <property name="method" type="string"/>
            <property name="link-local" type="int32"/>
          </setting>
        </nm-setting-docs>"#;
        let settings = crate::settings::parse(table, &[]).unwrap();
        let constants = [
            constant("SETTING_IP4_CONFIG_SETTING_NAME", "ipv4"),
            constant("SETTING_IP4_CONFIG_METHOD_AUTO", "auto"),
            // A value whose text is also the name of another property of the same setting: it
            // is the constant's name that says which it is, not its value.
            constant("SETTING_IP4_CONFIG_METHOD_LINK_LOCAL", "link-local"),
            // And the property itself, under this prefix, which must not become a value.
            constant("SETTING_IP4_CONFIG_LINK_LOCAL", "link-local"),
        ];

        let sets = derive(&constants, &settings);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].rust_name, "Ipv4Method");
        let values: Vec<_> = sets[0].values.iter().map(|v| v.text.as_str()).collect();
        assert_eq!(values, ["auto", "link-local"]);
    }

    #[test]
    fn takes_the_longest_property_a_value_extends() {
        let mut constants = constants();
        constants.push(constant("SETTING_WIRELESS_MODE_BAND", "should-not-happen"));
        constants.push(constant("SETTING_WIRELESS_BAND", "band"));
        constants.push(constant("SETTING_WIRELESS_BAND_A", "a"));

        let sets = derive(&constants, &settings());
        let band = sets
            .iter()
            .find(|s| s.property.as_deref() == Some("band"))
            .unwrap();
        assert_eq!(band.values.len(), 1);
        assert_eq!(band.values[0].text, "a");
    }

    #[test]
    fn hand_written_values_cannot_repeat_declared_ones() {
        let annotated = [Enumeration::property("802-11-wireless", "mode", Vec::new())];
        let error = resolve(derive(&constants(), &settings()), &annotated, &settings())
            .unwrap_err()
            .to_string();
        assert!(error.contains("WirelessMode"), "{error}");
    }

    #[test]
    fn hand_written_values_must_be_of_a_string_property() {
        let annotated = [Enumeration::property("802-11-wireless", "ssid", Vec::new())];
        assert!(resolve(Vec::new(), &annotated, &settings()).is_err());
    }

    #[test]
    fn a_newtype_must_match_the_wire_type() {
        let mut settings = settings();
        let ok = [Newtype::profile("802-11-wireless", "ssid", "Ssid")];
        apply_newtypes(&mut settings, &ok).unwrap();
        let ssid = find(&settings, "802-11-wireless", "ssid").unwrap();
        assert_eq!(
            ssid.ty,
            PropertyType::Newtype {
                name: "Ssid".to_owned()
            }
        );

        // `mode` is a string, and `Ssid` reads a byte array.
        let wrong = [Newtype::profile("802-11-wireless", "mode", "Ssid")];
        assert!(apply_newtypes(&mut settings, &wrong).is_err());

        let unknown = [Newtype::profile("802-11-wireless", "ssid", "Quaternion")];
        assert!(apply_newtypes(&mut settings, &unknown).is_err());
    }

    #[test]
    fn an_interface_newtype_must_match_the_signature() {
        let interfaces = [(
            "org.freedesktop.NetworkManager.AccessPoint".to_owned(),
            "Ssid".to_owned(),
            "ay".to_owned(),
        )];
        let entry = |interface: &str, name: &str, ty: &str| {
            toml::from_str::<Newtype>(&format!(
                "interface = \"{interface}\"\nname = \"{name}\"\ntype = \"{ty}\"\n"
            ))
            .expect("the entry parses")
        };

        let ok = [entry(
            "org.freedesktop.NetworkManager.AccessPoint",
            "Ssid",
            "Ssid",
        )];
        check_member_newtypes(&ok, &interfaces).unwrap();

        // `AssignedMacAddress` reads a string, and the property travels as `ay`.
        let wrong = [entry(
            "org.freedesktop.NetworkManager.AccessPoint",
            "Ssid",
            "AssignedMacAddress",
        )];
        assert!(check_member_newtypes(&wrong, &interfaces).is_err());

        let missing = [entry(
            "org.freedesktop.NetworkManager.AccessPoint",
            "Gone",
            "Ssid",
        )];
        assert!(check_member_newtypes(&missing, &interfaces).is_err());
    }
}

#[cfg(test)]
mod vendored {
    /// The real spec derives exactly the value sets NetworkManager declares constants for.
    ///
    /// Pinned as a list because the rules that pick these out are the delicate part: a change
    /// upstream that makes them read a property as a value of another would show up here as a
    /// set nobody meant, and one that adds a genuine set is a one-line update.
    #[test]
    fn derives_what_the_constants_declare() {
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
        let gir = std::fs::read_to_string(root.join("spec/NM-1.0.gir")).expect("the spec is here");
        let table = std::fs::read_to_string(root.join("spec/nm-settings-docs-dbus.xml"))
            .expect("the table is here");
        let overrides = crate::overrides::load(root).expect("annotations.toml parses");

        let settings = crate::settings::parse(&table, &overrides.setting_fields()).unwrap();
        let constants = crate::gir::parse_constants(&gir).unwrap();

        let derived: Vec<String> = super::derive(&constants, &settings)
            .iter()
            .map(|set| format!("{} -> {}", set.describe(), set.rust_name))
            .collect();
        assert_eq!(
            derived,
            [
                "802-11-wireless.mode -> WirelessMode",
                "adsl.encapsulation -> AdslEncapsulation",
                "adsl.protocol -> AdslProtocol",
                "bluetooth.type -> BluetoothType",
                "ipv4.method -> Ipv4Method",
                "ipv6.method -> Ipv6Method",
                "team.runner -> TeamRunner",
                "team.runner-agg-select-policy -> TeamRunnerAggSelectPolicy",
                "team.runner-hwaddr-policy -> TeamRunnerHwaddrPolicy",
            ]
        );
    }
}
