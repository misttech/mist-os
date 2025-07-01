// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::id::IdExt;
use crate::ir::Id;

use super::{is_compat_reserved, is_reserved};

pub fn camel(id: &Id) -> askama::Result<String> {
    Ok(ident(id.camel()))
}

pub fn snake(id: &Id) -> askama::Result<String> {
    Ok(ident(id.snake()))
}

pub fn screaming_snake(id: &Id) -> askama::Result<String> {
    Ok(ident(id.screaming_snake()))
}

pub fn compat_snake(id: &Id) -> askama::Result<String> {
    Ok(compat_ident(id.snake(), id))
}

pub fn compat_camel(id: &Id) -> askama::Result<String> {
    Ok(compat_ident(id.camel(), id))
}

fn ident(mut name: String) -> String {
    if is_reserved(&name) {
        name.push('_');
    }
    name
}

fn compat_ident(mut name: String, id: &Id) -> String {
    if is_compat_reserved(id.non_canonical()) {
        name.push('_');
    }
    name
}
