// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::r#type::Type;
use super::{Attributes, CompIdent, Ident};
use crate::de::Index;

#[derive(Clone, Debug, Deserialize)]
pub struct Protocol {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    #[expect(dead_code)]
    pub composed_protocols: Vec<ComposedProtocol>,
    pub methods: Vec<ProtocolMethod>,
    pub openness: ProtocolOpenness,
}

impl Index for Protocol {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
    }
}

impl Protocol {
    pub fn transport(&self) -> Option<&str> {
        self.attributes
            .attributes
            .get("transport")
            .and_then(|attr| attr.args.get("value"))
            .map(|arg| arg.value.value.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolOpenness {
    Open,
    Ajar,
    Closed,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProtocolMethod {
    #[serde(flatten)]
    pub attributes: Attributes,
    #[expect(dead_code)]
    pub has_request: bool,
    #[expect(dead_code)]
    pub has_response: bool,
    #[expect(dead_code)]
    pub is_composed: bool,
    pub has_error: bool,
    pub kind: ProtocolMethodKind,
    pub maybe_request_payload: Option<Box<Type>>,
    pub maybe_response_payload: Option<Box<Type>>,
    pub maybe_response_success_type: Option<Box<Type>>,
    pub maybe_response_err_type: Option<Box<Type>>,
    pub name: Ident,
    pub ordinal: u64,
    #[serde(rename = "strict")]
    pub is_strict: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolMethodKind {
    OneWay,
    TwoWay,
    Event,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ComposedProtocol {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    #[expect(dead_code)]
    pub name: CompIdent,
}
