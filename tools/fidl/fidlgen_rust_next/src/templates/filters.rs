// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::id::IdExt;
use crate::ir::Id;

use super::reserved::escape;

pub fn camel(id: &Id) -> askama::Result<String> {
    Ok(escape(id.camel()))
}

pub fn snake(id: &Id) -> askama::Result<String> {
    Ok(escape(id.snake()))
}

pub fn screaming_snake(id: &Id) -> askama::Result<String> {
    Ok(escape(id.screaming_snake()))
}
