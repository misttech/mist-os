// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module implements the conversion between the IR representation and the
//! comparison representation.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use anyhow::{anyhow, bail, Context as _, Result};
use flyweights::FlyStr;
use itertools::Itertools;

use crate::compare;
use crate::ir;

#[derive(Clone)]
pub struct Context {
    ir: Rc<ir::IR>,
    identifier_stack: Vec<FlyStr>,
    path: compare::Path,
}

impl Context {
    pub fn new(ir: Rc<ir::IR>, api_level: FlyStr) -> Self {
        Self { ir, identifier_stack: vec![], path: compare::Path::new(&api_level) }
    }
    pub fn nest_member(&self, member_name: impl AsRef<str>, identifier: Option<FlyStr>) -> Self {
        let mut context = self.clone();
        context.path.push(compare::PathElement::Member(
            FlyStr::new(member_name.as_ref()),
            identifier.clone(),
        ));
        if let Some(identifier) = identifier {
            context.identifier_stack.push(identifier);
        }
        context
    }

    pub fn nest_list(&self, member_identifier: Option<FlyStr>) -> Self {
        let mut context = self.clone();
        context.path.push(compare::PathElement::List(member_identifier.clone()));
        if let Some(identifier) = member_identifier {
            context.identifier_stack.push(identifier);
        }
        context
    }

    pub fn get(&self, name: &str) -> Result<ir::Declaration> {
        self.ir.get(name)
    }

    /// Look for the supplied identifier in the identifier stack and if found return the length of the cycle to the last one.
    fn find_identifier_cycle(&self, name: impl AsRef<str>) -> Option<usize> {
        let name = name.as_ref();
        self.identifier_stack.iter().rev().skip(1).positions(|i| i == name).next()
    }

    #[cfg(test)]
    fn empty_for_test() -> Self {
        Self {
            ir: ir::IR::empty_for_tests(),
            identifier_stack: vec![],
            path: compare::Path::empty(),
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn test_find_identifier_cycle() {
        // Set up identifier stack
        let context = Context::empty_for_test()
            .nest_member("A", Some("foo".into()))
            .nest_member("B", Some("bar".into()))
            .nest_member("C", Some("baz".into()))
            .nest_member("D", Some("foo".into()))
            .nest_member("E", Some("quux".into()));
        assert_eq!(
            vec![
                FlyStr::new("foo"),
                FlyStr::new("bar"),
                FlyStr::new("baz"),
                FlyStr::new("foo"),
                FlyStr::new("quux")
            ],
            context.identifier_stack
        );

        // Check that it's as expected
        assert_eq!(None, context.find_identifier_cycle("blah"));
        assert_eq!(None, context.find_identifier_cycle("quux"));
        assert_eq!(Some(0), context.find_identifier_cycle("foo"));
        assert_eq!(Some(1), context.find_identifier_cycle("baz"));
        assert_eq!(Some(2), context.find_identifier_cycle("bar"));
    }
}

pub trait ConvertType {
    fn identifier(&self) -> Option<FlyStr>;
    fn convert(&self, context: Context) -> Result<compare::Type>;
}

impl ConvertType for ir::Type {
    fn identifier(&self) -> Option<FlyStr> {
        match self {
            ir::Type::Request { protocol_transport: _, subtype, nullable: _ } => {
                Some(FlyStr::new(subtype))
            }
            ir::Type::Identifier { identifier, nullable: _, protocol_transport: _ } => {
                Some(FlyStr::new(identifier))
            }
            _ => None,
        }
    }

    fn convert(&self, context: Context) -> Result<compare::Type> {
        Ok(match self {
            ir::Type::Array { element_count, element_type } => {
                let element_type =
                    Box::new(element_type.convert(context.nest_list(element_type.identifier()))?);
                compare::Type::Array(context.path, *element_count, element_type)
            }
            ir::Type::StringArray { element_count } => {
                compare::Type::StringArray(context.path, *element_count)
            }
            ir::Type::Vector { element_type, maybe_element_count, nullable } => {
                let element_type =
                    Box::new(element_type.convert(context.nest_list(element_type.identifier()))?);
                compare::Type::Vector(
                    context.path,
                    maybe_element_count.unwrap_or(0xFFFF),
                    element_type,
                    convert_nullable(nullable),
                )
            }
            ir::Type::String { maybe_element_count, nullable } => compare::Type::String(
                context.path,
                maybe_element_count.unwrap_or(0xFFFF),
                convert_nullable(nullable),
            ),
            ir::Type::Handle { nullable, subtype, rights } => compare::Type::Handle(
                context.path,
                convert_handle_type(subtype)?,
                convert_nullable(nullable),
                crate::compare::HandleRights::from_bits(*rights)
                    .ok_or_else(|| anyhow!("invalid handle rights bits 0x{:x}", *rights))?,
            ),
            ir::Type::Request { protocol_transport, subtype, nullable } => {
                let decl = context.get(subtype)?;
                if let ir::Declaration::Protocol(protocol) = decl {
                    compare::Type::ServerEnd(
                        context.path.clone(),
                        protocol.name.clone(),
                        protocol_transport.into(),
                        convert_nullable(nullable),
                        Box::new(convert_protocol(&protocol, context)?),
                    )
                } else {
                    panic!("Got server_end for {decl:?}");
                }
            }
            ir::Type::Identifier { identifier, nullable, protocol_transport } => {
                if let Some(cycle) = context.find_identifier_cycle(identifier) {
                    compare::Type::Cycle(context.path, identifier.clone(), cycle + 1)
                } else {
                    let decl = context.get(identifier)?;
                    if let ir::Declaration::Protocol(protocol) = decl {
                        compare::Type::ClientEnd(
                            context.path.clone(),
                            protocol.name.clone(),
                            protocol_transport.into(),
                            convert_nullable(nullable),
                            Box::new(convert_protocol(&protocol, context)?),
                        )
                    } else {
                        assert!(protocol_transport.is_none());
                        let path = context.path.clone();
                        let t = match decl {
                            ir::Declaration::Bits(decl) => decl.convert(context)?,
                            ir::Declaration::Enum(decl) => decl.convert(context)?,
                            ir::Declaration::Struct(decl) => decl.convert(context)?,
                            ir::Declaration::Table(decl) => decl.convert(context)?,
                            ir::Declaration::Union(decl) => decl.convert(context)?,
                            ir::Declaration::Protocol(_) => panic!("Handled in the if let above."),
                        };

                        if *nullable {
                            compare::Type::Box(path, Box::new(t))
                        } else {
                            t
                        }
                    }
                }
            }

            ir::Type::Internal { subtype } => match subtype.as_str() {
                "framework_error" => compare::Type::FrameworkError(context.path),
                _ => bail!("Unimplemented internal type: {subtype:?}"),
            },
            ir::Type::Primitive { subtype } => {
                compare::Type::Primitive(context.path, convert_primitive_subtype(subtype.as_str())?)
            }
        })
    }
}

impl TryInto<compare::Primitive> for ir::Type {
    type Error = anyhow::Error;

    fn try_into(self) -> std::result::Result<compare::Primitive, Self::Error> {
        match self {
            ir::Type::Primitive { subtype } => convert_primitive_subtype(subtype.as_str()),
            _ => bail!("Expected primitive, got {:?}", self),
        }
    }
}

impl ConvertType for ir::BitsDeclaration {
    fn identifier(&self) -> Option<FlyStr> {
        Some(FlyStr::new(&self.name))
    }
    fn convert(&self, context: Context) -> Result<compare::Type> {
        let mut members = BTreeSet::new();
        for m in &self.members {
            members.insert(m.value.integer_value()?);
        }
        let t = self.r#type.clone().try_into()?;
        Ok(compare::Type::Bits(context.path, convert_strict(self.strict), t, members))
    }
}

impl ConvertType for ir::EnumDeclaration {
    fn identifier(&self) -> Option<FlyStr> {
        Some(FlyStr::new(&self.name))
    }
    fn convert(&self, context: Context) -> Result<compare::Type> {
        let mut members = BTreeSet::new();
        for m in &self.members {
            members.insert(m.value.integer_value()?);
        }
        let t = convert_primitive_subtype(self.r#type.as_str())?;
        Ok(compare::Type::Enum(context.path, convert_strict(self.strict), t, members))
    }
}

impl ConvertType for ir::TableDeclaration {
    fn identifier(&self) -> Option<FlyStr> {
        Some(FlyStr::new(&self.name))
    }
    fn convert(&self, context: Context) -> Result<compare::Type> {
        let mut members = BTreeMap::new();
        for m in &self.members {
            let t = &m.r#type;
            members.insert(m.ordinal, t.convert(context.nest_member(&m.name, t.identifier()))?);
        }
        Ok(compare::Type::Table(context.path, members))
    }
}

impl ConvertType for ir::StructDeclaration {
    fn identifier(&self) -> Option<FlyStr> {
        Some(FlyStr::new(&self.name))
    }
    fn convert(&self, context: Context) -> Result<compare::Type> {
        let members = self
            .members
            .iter()
            .map(|m| m.r#type.convert(context.nest_member(&m.name, m.r#type.identifier())))
            .collect::<Result<_>>()?;
        Ok(compare::Type::Struct(context.path, members))
    }
}

impl ConvertType for ir::UnionDeclaration {
    fn identifier(&self) -> Option<FlyStr> {
        Some(FlyStr::new(&self.name))
    }
    fn convert(&self, context: Context) -> Result<compare::Type> {
        let mut members = BTreeMap::new();
        for m in &self.members {
            let t = &m.r#type;
            members.insert(m.ordinal, t.convert(context.nest_member(&m.name, t.identifier()))?);
        }
        Ok(compare::Type::Union(context.path, convert_strict(self.strict), members))
    }
}

impl ConvertType for ir::Declaration {
    fn identifier(&self) -> Option<FlyStr> {
        match self {
            ir::Declaration::Bits(decl) => decl.identifier(),
            ir::Declaration::Enum(decl) => decl.identifier(),
            ir::Declaration::Protocol(decl) => Some(FlyStr::new(&decl.name)),
            ir::Declaration::Struct(decl) => decl.identifier(),
            ir::Declaration::Table(decl) => decl.identifier(),
            ir::Declaration::Union(decl) => decl.identifier(),
        }
    }

    fn convert(&self, context: Context) -> Result<compare::Type> {
        match self {
            ir::Declaration::Bits(decl) => decl.convert(context),
            ir::Declaration::Enum(decl) => decl.convert(context),
            ir::Declaration::Protocol(decl) => Ok(compare::Type::ClientEnd(
                context.path.clone(),
                decl.name.clone(),
                compare::Transport::Channel,    /* TODO */
                compare::Optionality::Required, /* TODO */
                Box::new(convert_protocol(decl, context)?),
            )),
            ir::Declaration::Struct(decl) => decl.convert(context),
            ir::Declaration::Table(decl) => decl.convert(context),
            ir::Declaration::Union(decl) => decl.convert(context),
        }
    }
}

fn convert_nullable(nullable: &bool) -> compare::Optionality {
    use compare::Optionality::*;
    if *nullable {
        Optional
    } else {
        Required
    }
}

fn convert_strict(strict: bool) -> compare::Flexibility {
    use compare::Flexibility::*;
    if strict {
        Strict
    } else {
        Flexible
    }
}

fn convert_primitive_subtype(subtype: &str) -> Result<compare::Primitive> {
    use compare::Primitive::*;
    Ok(match subtype {
        "bool" => Bool,
        "int8" => Int8,
        "uint8" => Uint8,
        "int16" => Int16,
        "uint16" => Uint16,
        "int32" => Int32,
        "uint32" => Uint32,
        "int64" => Int64,
        "uint64" => Uint64,
        "float32" => Float32,
        "float64" => Float64,
        _ => bail!("Unsupported primitive subtype: {}", subtype),
    })
}

fn convert_handle_type(subtype: impl AsRef<str>) -> Result<Option<compare::HandleType>> {
    use compare::HandleType::*;
    let subtype = subtype.as_ref();
    Ok(match subtype {
        "" => None,
        // TODO: actually convert
        _ => Some(Channel),
    })
}

/// Convert an Option<ir::Type> to either the appropriate compare type, or an empty struct.
fn maybe_convert_type(maybe_type: &Option<ir::Type>, context: Context) -> Result<compare::Type> {
    match maybe_type {
        Some(t) => Ok(t.convert(context)?),
        None => Ok(compare::Type::Struct(context.path, vec![])),
    }
}

fn maybe_type_identifier(maybe_type: &Option<ir::Type>) -> Option<FlyStr> {
    if let Some(t) = maybe_type {
        t.identifier()
    } else {
        None
    }
}

fn convert_method(method: &ir::ProtocolMethod, context: Context) -> Result<compare::Method> {
    let context = context.nest_member(method.name.clone(), None);
    let flexibility = convert_strict(method.strict);
    let path = context.path.clone();
    Ok(match (method.has_request, method.has_response) {
        (true, true) => compare::Method::two_way(
            path,
            flexibility,
            maybe_convert_type(
                &method.maybe_request_payload,
                context
                    .nest_member("REQUEST", maybe_type_identifier(&method.maybe_request_payload)),
            )?,
            maybe_convert_type(
                &method.maybe_response_payload,
                context
                    .nest_member("RESPONSE", maybe_type_identifier(&method.maybe_response_payload)),
            )?,
        ),
        (true, false) => compare::Method::one_way(
            path,
            flexibility,
            maybe_convert_type(
                &method.maybe_request_payload,
                context
                    .nest_member("REQUEST", maybe_type_identifier(&method.maybe_request_payload)),
            )?,
        ),
        (false, true) => compare::Method::event(
            path,
            flexibility,
            maybe_convert_type(
                &method.maybe_response_payload,
                context
                    .nest_member("PAYLOAD", maybe_type_identifier(&method.maybe_response_payload)),
            )?,
        ),
        (false, false) => panic!("Invalid IR"),
    })
}

fn convert_protocol(p: &ir::ProtocolDeclaration, context: Context) -> Result<compare::Protocol> {
    let mut methods = BTreeMap::new();

    let context = context.nest_member(&p.name, Some(FlyStr::new(&p.name)));

    for pm in &p.methods {
        methods.insert(
            pm.ordinal,
            convert_method(pm, context.clone())
                .with_context(|| format!("Method {}.{}", &p.name, &pm.name))?,
        );
    }

    let discoverable = ir::get_attribute(&p.maybe_attributes, "discoverable").map(|discoverable| {
        let attr_or = |name: &str, default: &str| {
            discoverable
                .get_argument(name)
                .map(|c| c.value().to_string())
                .unwrap_or(default.to_string())
        };
        let impl_locs = |name: &str| {
            let locs: Vec<String> = attr_or(name, "platform,external")
                .split(",")
                .into_iter()
                .map(|l| l.trim().into())
                .collect();
            compare::ImplementationLocation {
                platform: locs.contains(&"platform".to_string()),
                external: locs.contains(&"external".to_string()),
            }
        };
        compare::Discoverable {
            name: attr_or("name", &p.name),
            client: impl_locs("client"),
            server: impl_locs("server"),
        }
    });

    Ok(compare::Protocol {
        name: FlyStr::new(&p.name),
        path: context.path.clone(),
        openness: p.openness,
        methods,
        discoverable,
    })
}

pub fn convert_platform(ir: Rc<ir::IR>) -> Result<compare::Platform> {
    let mut platform = compare::Platform::default();
    platform.api_level = match ir.available.get("fuchsia") {
        None => bail!("missing API level for 'fuchsia'"),
        Some(api_level) => FlyStr::new(api_level),
    };

    let context = Context::new(ir.clone(), platform.api_level.clone());
    for decl in &ir.protocol_declarations {
        let protocol = convert_protocol(decl, context.clone())?;

        if let Some(discoverable) = protocol.discoverable.clone() {
            platform.discoverable.insert(discoverable.name, protocol);
        } else {
            platform.tear_off.insert(decl.name.clone(), protocol);
        };
    }

    Ok(platform)
}
