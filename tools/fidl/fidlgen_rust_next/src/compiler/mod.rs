// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod natural;
mod query;
mod resource_binding;
mod util;
mod wire;

use std::collections::HashMap;
use std::io::{Error, Write};

use self::query::{Properties, Property};
use crate::ir::{CompIdent, DeclType, Schema};

pub use self::resource_binding::ResourceBindings;

pub struct Config {
    pub emit_debug_impls: bool,
    pub resource_bindings: ResourceBindings,
}

pub struct Compiler<'a> {
    schema: &'a Schema,
    config: Config,
    type_to_properties: HashMap<CompIdent, Properties>,
}

impl<'a> Compiler<'a> {
    pub fn new(schema: &'a Schema, config: Config) -> Self {
        Self { schema, config, type_to_properties: HashMap::new() }
    }

    fn calculate<P: Property>(&mut self, ident: &CompIdent) -> P::Type {
        match self.schema.declarations[ident] {
            DeclType::Struct => {
                let s = self
                    .schema
                    .struct_declarations
                    .get(ident)
                    .or_else(|| self.schema.external_struct_declarations.get(ident))
                    .expect("undeclared struct");
                P::calculate_struct(self, s)
            }
            DeclType::Table => {
                let table = &self.schema.table_declarations[ident];
                P::calculate_table(self, table)
            }
            DeclType::Enum => {
                let enm = &self.schema.enum_declarations[ident];
                P::calculate_enum(self, enm)
            }
            DeclType::Union => {
                let union = &self.schema.union_declarations[ident];
                P::calculate_union(self, union)
            }
            _ => todo!(),
        }
    }

    pub fn query<P: Property>(&mut self, ident: &CompIdent) -> P::Type {
        let properties = self.type_to_properties.entry(ident.clone()).or_default();
        if let Some(result) = P::select(properties) {
            return *result;
        }

        let result = self.calculate::<P>(ident);
        let properties = self.type_to_properties.get_mut(ident).unwrap();
        *P::select(properties) = Some(result);
        result
    }

    pub fn emit<W: Write>(&mut self, out: &mut W) -> Result<(), Error> {
        writeln!(
            out,
            r#"
            // DO NOT EDIT: This file is machine-generated by fidlgen
            #![warn(clippy::all)]
            #![allow(
                unused_parens,
                unused_variables,
                unused_mut,
                unused_imports,
                unreachable_code,
                nonstandard_style,
            )]
            "#,
        )?;

        for ident in &self.schema.declaration_order {
            match self.schema.declarations[ident] {
                DeclType::Struct => {
                    natural::emit_struct(self, out, ident)?;
                    wire::emit_struct(self, out, ident)?;
                }
                DeclType::Table => {
                    natural::emit_table(self, out, ident)?;
                    wire::emit_table(self, out, ident)?;
                }
                DeclType::Enum => {
                    natural::emit_enum(self, out, ident)?;
                    wire::emit_enum(self, out, ident)?;
                }
                DeclType::Union => {
                    natural::emit_union(self, out, ident)?;
                    wire::emit_union(self, out, ident)?;
                }
                _ => (),
            }
        }

        Ok(())
    }
}
