// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ir::Declaration;

use crate::compare::problems::CompatibilityProblems;
use crate::compare::{Platform, Protocol, Type};
use crate::convert::{Context, ConvertType};
use crate::ir::IR;
use flyweights::FlyStr;
use std::rc::Rc;

// API levels of interest
const V1: &str = "1";
const V2: &str = "2";
const PLATFORM: &str = "1,2,NEXT,HEAD";

// Test library name
pub const LIBRARY: &str = "fuchsia.compat.test";

struct TestLibrary {
    source: String,
}

impl TestLibrary {
    fn new_added_at(added: &str, source_fragment: &str) -> Self {
        Self {
            source: format!(
                "
        @available(added={added})
        library {LIBRARY};

        {source_fragment}
        "
            ),
        }
    }
    fn new(source_fragment: &str) -> Self {
        Self::new_added_at(V1, source_fragment)
    }

    fn scope_name(&self, name: &str) -> String {
        format!("{LIBRARY}/{name}")
    }

    fn compile(&self, api_level: &str) -> CompiledTestLibrary {
        CompiledTestLibrary::new(api_level, self)
    }
}

struct CompiledTestLibrary {
    api_level: String,
    source: String,
    ir: Rc<IR>,
    context: Context,
}

impl CompiledTestLibrary {
    fn new(api_level: &str, library: &TestLibrary) -> Self {
        let ir = IR::from_source(api_level, &library.source, LIBRARY)
            .unwrap_or_else(|_| panic!("Compiling {api_level:?}:\n{0}", library.source));
        let context = Context::new(ir.clone(), FlyStr::new(api_level));
        Self { api_level: api_level.to_string(), source: library.source.clone(), ir, context }
    }

    fn get_decl(&self, name: &str) -> Declaration {
        self.ir.get(name).unwrap_or_else(|_| {
            panic!(
                "Couldn't find declaration {:?} at {:?} in:\n{}",
                name, self.api_level, self.source,
            )
        })
    }

    fn get_type(&self, name: &str) -> Type {
        let decl = self.get_decl(name);
        let context = self.context.nest_member(name, decl.identifier());
        use crate::convert::ConvertType;
        decl.convert(context).unwrap_or_else(|_| {
            panic!("Couldn't convert {name} to a type at {:?} in:\n{}", self.api_level, self.source,)
        })
    }

    fn get_protocol(&self, name: &str) -> Protocol {
        let decl = self.get_decl(name);
        let context = self.context.nest_member(name, decl.identifier());

        if let Declaration::Protocol(protocol) = decl {
            crate::convert::convert_protocol(&protocol, context).unwrap_or_else(|_| {
                panic!(
                    "Couldn't convert {name} to a protocol at {:?} in:\n{}",
                    self.api_level, self.source,
                )
            })
        } else {
            panic!("{name} is not a protocol at {:?} in:\n{}", self.api_level, self.source)
        }
    }

    fn get_platform(&self) -> Platform {
        crate::convert::convert_platform(self.ir.clone()).unwrap_or_else(|_| {
            panic!("Converting platform at {:?}:\n{}", self.api_level, self.source)
        })
    }
}

#[derive(Clone, Copy)]
pub struct Versions {
    pub external: &'static str,
    pub platform: &'static str,
}

#[allow(unused)]
pub(super) fn compare_fidl_library(
    versions: Versions,
    source_fragment: impl AsRef<str>,
) -> CompatibilityProblems {
    let lib = TestLibrary::new(source_fragment.as_ref());

    let external = lib.compile(versions.external);
    let platform = lib.compile(versions.platform);
    super::compatible(&external.get_platform(), &platform.get_platform()).unwrap_or_else(|_| {
        panic!("Comparing {:?} and {:?}:\n{}", versions.external, versions.platform, lib.source)
    })
}

pub(super) fn compare_fidl_type(name: &str, source_fragment: &str) -> CompatibilityProblems {
    let lib = TestLibrary::new(source_fragment);
    let name = lib.scope_name(name);

    let ir1 = lib.compile(V1);
    let ir2 = lib.compile(V2);

    let t1 = ir1.get_type(&name);
    let t2 = ir2.get_type(&name);

    use super::compare_types;
    let mut problems = compare_types(&t1, &t2);
    problems.append(compare_types(&t2, &t1));
    problems
}

pub(super) fn compare_fidl_protocol(name: &str, source_fragment: &str) -> CompatibilityProblems {
    let lib = TestLibrary::new(source_fragment);
    let name = lib.scope_name(name);

    let external = lib.compile(V1).get_protocol(&name);
    let platform = lib.compile(PLATFORM).get_protocol(&name);

    Protocol::compatible(&external, &platform).unwrap_or_else(|_| {
        panic!(
            "Protocol compatibility for {:?} between {:?} and {:?}:\n{}",
            name, V1, PLATFORM, lib.source
        )
    })
}
