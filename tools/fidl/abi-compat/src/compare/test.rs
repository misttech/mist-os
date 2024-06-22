// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ir::Declaration;

use super::problems::CompatibilityProblems;
use crate::compare::{Protocol, Type};
use crate::convert::{Context, ConvertType};
use crate::ir::IR;
use anyhow::{bail, Context as _, Result};
use flyweights::FlyStr;
use std::rc::Rc;

const V1: &str = "1";
const V2: &str = "2";
const LIBRARY: &str = "fuchsia.compat.test";

struct TestLibrary {
    source: String,
}

impl TestLibrary {
    fn new(source_fragment: &str) -> Self {
        Self {
            source: format!(
                "
        @available(added={V1})
        library {LIBRARY};

        {source_fragment}
        "
            ),
        }
    }

    fn scope_name(&self, name: &str) -> String {
        format!("{LIBRARY}/{name}")
    }

    fn compile(&self) -> Result<(CompiledTestLibrary, CompiledTestLibrary)> {
        Ok((CompiledTestLibrary::new(&V1, self)?, CompiledTestLibrary::new(&V2, self)?))
    }
}

struct CompiledTestLibrary {
    ir: Rc<IR>,
    context: Context,
}

impl CompiledTestLibrary {
    fn new(api_level: &str, library: &TestLibrary) -> Result<Self> {
        assert!(api_level == V1 || api_level == V2);
        let ir = IR::from_source(api_level, &library.source, LIBRARY)
            .context(format!("Loading at fuchsia:{api_level}:\n{0}", library.source))?;
        let context = Context::new(ir.clone(), FlyStr::new(api_level));
        Ok(Self { ir, context })
    }

    fn get_decl(&self, name: &str) -> Result<Declaration> {
        self.ir.get(name)
    }

    fn get_type(&self, name: &str) -> Result<Type> {
        let decl = self.get_decl(name)?;
        let context = self.context.nest_member(name, decl.identifier());
        use crate::convert::ConvertType;
        decl.convert(context)
    }

    fn get_protocol(&self, name: &str) -> Result<Protocol> {
        let decl = self.get_decl(name)?;
        let context = self.context.nest_member(name, decl.identifier());

        if let Declaration::Protocol(protocol) = decl {
            crate::convert::convert_protocol(&protocol, context)
        } else {
            bail!("{name} is not a protocol")
        }
    }
}

pub(super) fn compare_fidl_type(name: &str, source_fragment: &str) -> CompatibilityProblems {
    let lib = TestLibrary::new(source_fragment);
    let name = lib.scope_name(name);

    let (ir1, ir2) = lib.compile().expect("Compiling test library");

    let t1 = ir1.get_type(&name).expect("Getting type");
    let t2 = ir2.get_type(&name).expect("Getting type");

    use super::compare_types;
    let mut problems = compare_types(&t1, &t2);
    problems.append(compare_types(&t2, &t1));
    problems
}

pub(super) fn compare_fidl_protocol(name: &str, source_fragment: &str) -> CompatibilityProblems {
    let lib = TestLibrary::new(source_fragment);
    let name = lib.scope_name(name);

    let (ir1, ir2) = lib.compile().expect("Compiling test library");

    let p1 = ir1.get_protocol(&name).expect("Getting protocol");
    let p2 = ir2.get_protocol(&name).expect("Getting protocol");

    let mut problems = Protocol::compatible(&p1, &p2).expect("Protocol compatibility");
    problems.append(Protocol::compatible(&p2, &p1).expect("Protocol compatibility"));

    problems
}
