// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeSet;

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{
    CompIdent, Protocol, ProtocolMethod, ProtocolMethodKind, ProtocolOpenness, Struct, Type,
    TypeKind,
};
use crate::templates::reserved::escape;

#[derive(Template)]
#[template(path = "protocol.askama", whitespace = "preserve")]
pub struct ProtocolTemplate<'a> {
    protocol: &'a Protocol,
    context: Context<'a>,

    non_canonical_name: &'a str,
    protocol_name: String,
    module_name: String,

    client_sender_name: String,
    client_handler_name: String,

    server_sender_name: String,
    server_handler_name: String,
}

impl<'a> ProtocolTemplate<'a> {
    pub fn new(protocol: &'a Protocol, context: Context<'a>) -> Self {
        let base_name = protocol.name.decl_name().camel();

        Self {
            protocol,
            context,

            non_canonical_name: protocol.name.decl_name().non_canonical(),
            protocol_name: escape(protocol.name.decl_name().camel()),
            module_name: escape(protocol.name.decl_name().snake()),

            client_sender_name: escape(format!("{base_name}ClientSender")),
            client_handler_name: escape(format!("{base_name}ClientHandler")),

            server_sender_name: escape(format!("{base_name}ServerSender")),
            server_handler_name: escape(format!("{base_name}ServerHandler")),
        }
    }

    fn get_method_args_struct(&self, method: &ProtocolMethod) -> Option<&Struct> {
        match method.kind {
            ProtocolMethodKind::OneWay | ProtocolMethodKind::TwoWay => {
                if let Some(args) = &method.maybe_request_payload {
                    if let TypeKind::Identifier { identifier, .. } = &args.kind {
                        return self.schema().struct_declarations.get(identifier).or_else(|| {
                            self.schema().external_struct_declarations.get(identifier)
                        });
                    }
                }
            }
            ProtocolMethodKind::Event => {
                if !method.has_error {
                    if let Some(args) = &method.maybe_response_payload {
                        if let TypeKind::Identifier { identifier, .. } = &args.kind {
                            return self.schema().struct_declarations.get(identifier).or_else(
                                || self.schema().external_struct_declarations.get(identifier),
                            );
                        }
                    }
                }
            }
        }
        None
    }

    fn discoverable_name(&self) -> Option<String> {
        let attr = self.protocol.attributes.attributes.get("discoverable")?;
        if let Some(name) = attr.args.get("name") {
            Some(name.value.value.clone())
        } else {
            let (library, name) = self.protocol.name.split();
            Some(format!("{}.{}", library, name.camel()))
        }
    }

    fn prelude_method_type_idents(&self) -> BTreeSet<CompIdent> {
        let mut result = BTreeSet::new();

        fn get_identifier(ty: &Type) -> Option<CompIdent> {
            if let Type { kind: TypeKind::Identifier { identifier, .. }, .. } = ty {
                Some(identifier.clone())
            } else {
                None
            }
        }

        for method in self.protocol.methods.iter() {
            // We always include the request payload in the prelude if there is one
            if let Some(request) = method.maybe_request_payload.as_deref() {
                result.extend(get_identifier(request));
            }

            if let Some(success) = method.maybe_response_success_type.as_deref() {
                // The response type is a result, so we only want to include the success and error
                // types in the prelude
                result.extend(get_identifier(success));

                if let Some(error) = method.maybe_response_err_type.as_deref() {
                    result.extend(get_identifier(error));
                }
            } else if let Some(response) = method.maybe_response_payload.as_deref() {
                // The response type is not a result, so we want to include the response payload
                // type in the prelude
                result.extend(get_identifier(response));
            }
        }

        result
    }
}

impl<'a> Contextual<'a> for ProtocolTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}
