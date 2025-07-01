// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Templates generate a lot of code which have tendencies to trip lints.
#![expect(clippy::diverging_sub_expression, dead_code, unreachable_code)]

mod alias;
mod bits;
mod compat;
mod r#const;
mod constant;
mod context;
mod denylist;
mod doc_string;
mod r#enum;
mod filters;
mod id;
mod natural_type;
mod prim;
mod protocol;
mod reserved;
mod schema;
mod service;
mod r#struct;
mod table;
mod r#union;
mod wire_type;

use askama::Template;

use crate::config::Config;
use crate::ir::*;

use self::alias::*;
use self::bits::*;
use self::compat::*;
use self::constant::*;
use self::context::*;
use self::denylist::*;
use self::doc_string::*;
use self::id::*;
use self::natural_type::*;
use self::prim::*;
use self::protocol::*;
use self::r#const::*;
use self::r#enum::*;
use self::r#struct::*;
use self::r#union::*;
use self::reserved::*;
use self::schema::*;
use self::service::*;
use self::table::*;
use self::wire_type::*;

pub fn render_schema(schema: &Schema, config: &Config) -> Result<String, askama::Error> {
    let context = Context::new(schema, config);

    SchemaTemplate::new(context).render()
}
