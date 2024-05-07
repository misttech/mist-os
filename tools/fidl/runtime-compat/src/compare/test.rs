// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::problems::CompatibilityProblems;
use anyhow::Result;

pub(super) fn compare_fidl_type(name: &str, source_fragment: &str) -> CompatibilityProblems {
    use anyhow::Context as _;
    use flyweights::FlyStr;

    use crate::{
        compare::{compare_types, Type},
        convert::{Context, ConvertType},
        ir::IR,
    };

    const V1: &'static str = "1";
    const V2: &'static str = "2";
    const LIBRARY: &'static str = "fuchsia.compat.test";

    let library_source = format!(
        "
    @available(added={V1})
    library {LIBRARY};

    {source_fragment}
    "
    );

    fn get_type(api_level: &str, name: &str, library_source: &str) -> Result<Type> {
        let ir = IR::from_source(api_level, &library_source, LIBRARY)
            .context(format!("Loading at fuchsia:{api_level}:\n{library_source}"))?;
        let decl = ir.get(name).context(format!("Looking up {name} at {api_level}"))?;
        let context = Context::new(ir, FlyStr::new(api_level));
        let context = context.nest_member(name, decl.identifier());
        decl.convert(context)
    }

    let name = format!("{LIBRARY}/{name}");

    let t1: Type = get_type(V1, &name, &library_source).unwrap();
    let t2: Type = get_type(V2, &name, &library_source).unwrap();

    let mut problems = compare_types(&t1, &t2);
    problems.append(compare_types(&t2, &t1));
    problems
}
