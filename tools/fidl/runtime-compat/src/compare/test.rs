// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::CompatibilityProblems;
use anyhow::Result;
use itertools::Itertools;

#[cfg(test)]
pub(super) fn compare_fidl_library(
    a1: &str,
    a2: &str,
    source: &str,
    library_name: &str,
) -> Result<CompatibilityProblems> {
    use anyhow::Context;

    use crate::{compare::compatible, convert::convert_platform, ir::IR};

    let ir1 = IR::from_source(a1, source, library_name)
        .context(format!("Loading at fuchsia:{}:\n{}", a1, source))?;
    let ir2 = IR::from_source(a2, source, library_name)
        .context(format!("Loading at fuchsia:{}:\n{}", a2, source))?;

    let p1 = convert_platform(ir1).context("Converting platform")?;
    let p2 = convert_platform(ir2).context("Converting platform")?;

    println!("p1 discoverable: {:?}", p1.discoverable.iter().collect_vec());
    println!("p2 discoverable: {:?}", p2.discoverable.iter().collect_vec());

    compatible(&p1, &p2)
}

#[cfg(test)]
pub(super) fn compare_fidl_type(name: &str, source_fragment: &str) -> CompatibilityProblems {
    use anyhow::Context as _;
    use flyweights::FlyStr;

    use crate::{
        compare::{compare_types, CompatibilityProblems, Type},
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
