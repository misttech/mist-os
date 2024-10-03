// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{CompIdent, Literal};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Constant {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub kind: ConstantKind,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstantKind {
    Identifier {
        #[expect(dead_code)]
        identifier: CompIdent,
    },
    Literal {
        #[expect(dead_code)]
        literal: Literal,
    },
    BinaryOperator,
}
