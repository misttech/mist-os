// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::id::IdExt;
use crate::ir::Id;

use super::reserved::escape;

pub fn escape_camel(id: &Id) -> String {
    escape(id.camel())
}

pub fn escape_snake(id: &Id) -> String {
    escape(id.snake())
}

pub fn escape_screaming_snake(id: &Id) -> String {
    escape(id.screaming_snake())
}

pub fn camel(id: &Id, _: &dyn askama::Values) -> askama::Result<String> {
    Ok(escape_camel(id))
}

pub fn snake(id: &Id, _: &dyn askama::Values) -> askama::Result<String> {
    Ok(escape_snake(id))
}

pub fn screaming_snake(id: &Id, _: &dyn askama::Values) -> askama::Result<String> {
    Ok(escape_screaming_snake(id))
}
