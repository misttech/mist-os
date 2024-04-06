// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module is concerned with reading the FIDL JSON IR. It supports a subset
//! of the IR that is relevant to version compatibility comparisons.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::{collections::HashMap, fmt::Display, fs, io::BufReader, path::Path, rc::Rc};

#[derive(Deserialize, Debug, Clone)]
pub struct AttributeArgument {
    pub name: String,
    pub value: Constant,
}
#[derive(Deserialize, Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub arguments: Vec<AttributeArgument>,
}

pub fn get_attribute<S: AsRef<str>>(
    attributes: &Option<Vec<Attribute>>,
    name: S,
) -> Option<Vec<String>> {
    let name = name.as_ref();
    match attributes {
        Some(attrs) => match attrs.iter().filter(|a| a.name == name).next() {
            Some(attr) => {
                Some(attr.arguments.iter().map(|arg| arg.value.value().to_owned()).collect())
            }
            None => None,
        },
        None => None,
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Constant {
    Literal { value: String },
    Identifier { value: String },
}

impl Constant {
    pub fn value<'a>(&'a self) -> &'a str {
        match self {
            Constant::Literal { value } => value,
            Constant::Identifier { value } => value,
        }
    }
    pub fn integer_value(&self) -> Result<i128> {
        Ok(self
            .value()
            .parse()
            .with_context(|| format!("Parsing {:?} as integer constant.", self.value()))?)
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Type {
    Array { element_count: u64, element_type: Box<Type> },
    StringArray { element_count: u64 },
    Vector { element_type: Box<Type>, maybe_element_count: Option<u64>, nullable: bool },
    String { maybe_element_count: Option<u64>, nullable: bool },
    Handle { nullable: bool, subtype: String, rights: u32 },
    Request { protocol_transport: String, subtype: String, nullable: bool },
    Identifier { identifier: String, nullable: bool, protocol_transport: Option<String> },
    Internal { subtype: String },
    Primitive { subtype: String },
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitsMember {
    pub name: String,
    pub value: Constant,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitsDeclaration {
    pub name: String,
    pub members: Vec<BitsMember>,
    pub strict: bool,
    pub r#type: Type,
}

#[derive(Deserialize, Debug, Clone)]
pub struct EnumMember {
    pub name: String,
    pub value: Constant,
}

#[derive(Deserialize, Debug, Clone)]
pub struct EnumDeclaration {
    pub name: String,
    pub members: Vec<EnumMember>,
    pub strict: bool,
    pub r#type: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProtocolMethod {
    pub name: String,
    pub ordinal: u64,
    pub strict: bool,
    pub has_request: bool,
    pub has_response: bool,
    pub maybe_request_payload: Option<Type>,
    pub maybe_response_payload: Option<Type>,
}

impl ProtocolMethod {}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum Openness {
    Open,
    Ajar,
    Closed,
}
impl Display for Openness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Openness::Open => write!(f, "open"),
            Openness::Ajar => write!(f, "ajar"),
            Openness::Closed => write!(f, "closed"),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProtocolDeclaration {
    pub name: String,
    pub methods: Vec<ProtocolMethod>,
    pub openness: Openness,
    pub maybe_attributes: Option<Vec<Attribute>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TableMember {
    pub ordinal: u64,
    pub name: String,
    pub r#type: Type,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TableDeclaration {
    pub name: String,
    pub members: Vec<TableMember>,
    pub resource: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StructMember {
    pub name: String,
    pub r#type: Type,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StructDeclaration {
    pub name: String,
    pub members: Vec<StructMember>,
    pub resource: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UnionMember {
    pub ordinal: u64,
    pub name: String,
    pub r#type: Type,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UnionDeclaration {
    pub name: String,
    pub members: Vec<UnionMember>,
    pub resource: bool,
    pub strict: bool,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationKind {
    Alias,
    Bits,
    Const,
    Enum,
    ExperimentalResource,
    NewType,
    Overlay,
    Protocol,
    Service,
    Struct,
    Table,
    Union,
}

pub enum Declaration {
    Bits(BitsDeclaration),
    Enum(EnumDeclaration),
    Protocol(ProtocolDeclaration),
    Struct(StructDeclaration),
    Table(TableDeclaration),
    Union(UnionDeclaration),
}

#[derive(Deserialize, Debug)]
pub struct IR {
    pub available: HashMap<String, String>,
    bits_declarations: Vec<BitsDeclaration>,
    enum_declarations: Vec<EnumDeclaration>,
    pub protocol_declarations: Vec<ProtocolDeclaration>,
    struct_declarations: Vec<StructDeclaration>,
    table_declarations: Vec<TableDeclaration>,
    union_declarations: Vec<UnionDeclaration>,
    declarations: HashMap<String, DeclarationKind>,
}

impl IR {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Rc<Self>> {
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        Ok(Rc::new(serde_json::from_reader(reader)?))
    }

    #[cfg(test)]
    pub fn from_source(
        fuchsia_available: &str,
        fidl_source: &str,
        library_name: &str,
    ) -> Result<Rc<Self>> {
        testing::ir_from_source(fuchsia_available, fidl_source, library_name)
    }

    pub fn get(&self, name: &str) -> Result<Declaration> {
        let kind = self
            .declarations
            .get(name)
            .ok_or_else(|| anyhow!("Declaration not found: {}", name))?;
        use DeclarationKind::*;
        let declaration = match kind {
            Bits => Declaration::Bits(
                self.bits_declarations.iter().find(|&d| d.name == name).unwrap().clone(),
            ),
            Enum => Declaration::Enum(
                self.enum_declarations.iter().find(|&d| d.name == name).unwrap().clone(),
            ),
            Protocol => Declaration::Protocol(
                self.protocol_declarations.iter().find(|&d| d.name == name).unwrap().clone(),
            ),
            Struct => Declaration::Struct(
                self.struct_declarations.iter().find(|&d| d.name == name).unwrap().clone(),
            ),
            Table => Declaration::Table(
                self.table_declarations.iter().find(|&d| d.name == name).unwrap().clone(),
            ),
            Union => Declaration::Union(
                self.union_declarations.iter().find(|&d| d.name == name).unwrap().clone(),
            ),
            _ => bail!("Unsupported declaration kind ({:?}) for: {}", kind, name),
        };

        Ok(declaration)
    }

    #[cfg(test)]
    pub fn empty_for_tests() -> Rc<Self> {
        use maplit::hashmap;
        return Rc::new(Self {
            available: hashmap! {},
            bits_declarations: vec![],
            enum_declarations: vec![],
            protocol_declarations: vec![],
            struct_declarations: vec![],
            table_declarations: vec![],
            union_declarations: vec![],
            declarations: hashmap! {},
        });
    }
}

#[test]
fn test_load_simple_library() {
    use maplit::hashmap;
    let ir = IR::from_source(
        "1",
        "
@available(added=1)
library fuchsia.test.library;

type Foo = table {
    1: bar string;
    3: baz int32;
};

open protocol Example {
    flexible SetFoo(struct { foo Foo; });
    flexible GetFoo() -> (struct { foo Foo; }) error int32;
};
",
        "fuchsia.test.library",
    )
    .expect("loading simple test library");

    assert_eq!(hashmap! {"fuchsia".to_owned() => "1".to_owned()}, ir.available);

    assert_eq!(1, ir.table_declarations.len());
}

#[cfg(test)]
mod testing {
    use super::{Result, IR};
    use std::{ffi::OsString, path::Path, process::Command, rc::Rc};

    // A minimal zx library for tests.
    const ZX_SOURCE: &'static str = "
    library zx;

    type ObjType = enum : uint32 {
        NONE = 0;
        PROCESS = 1;
        THREAD = 2;
        VMO = 3;
        CHANNEL = 4;
        EVENT = 5;
        PORT = 6;
    };

    type Rights = bits : uint32 {
        DUPLICATE = 0x00000001;
        TRANSFER = 0x00000002;
        READ = 0x00000004;
        WRITE = 0x00000008;
        EXECUTE = 0x00000010;
    };

    resource_definition Handle : uint32 {
        properties {
            subtype ObjType;
            rights Rights;
        };
    };";

    fn path_to_fidlc() -> OsString {
        let argv0 = std::env::args().next().expect("Can't get argv[0]");
        let fidlc = Path::new(&argv0).parent().unwrap().join("fidlc");
        if !fidlc.is_file() {
            panic!("{:?} is not a file", fidlc);
        }
        fidlc.into()
    }
    pub fn ir_from_source(
        fuchsia_available: &str,
        fidl_source: &str,
        library_name: &str,
    ) -> Result<Rc<IR>> {
        let fidlc_path = path_to_fidlc();

        let dir = tempfile::tempdir().expect("Creating a temporary directory");
        let temp_path =
            |filename: &str| -> String { dir.path().join(filename).to_string_lossy().to_string() };
        let source_path = temp_path("test.fidl");
        let ir_path = temp_path("test.fidl.json");
        std::fs::write(&source_path, fidl_source).expect("Writing FIDL source for test");
        let mut args = vec![
            "--available".to_string(),
            format!("fuchsia:{}", fuchsia_available),
            "--json".to_string(),
            ir_path.clone(),
            "--name".to_string(),
            library_name.to_string(),
            "--platform".to_string(),
            "fuchsia".to_string(),
        ];

        if fidl_source.contains("using zx;") {
            let zx_path = temp_path("zx.fidl");
            std::fs::write(&zx_path, ZX_SOURCE).expect("Writing zx source for test");
            args.append(&mut vec!["--files".to_string(), zx_path])
        }

        args.append(&mut vec!["--files".to_string(), source_path]);

        let status = Command::new(fidlc_path).args(args).status().expect("Failed to run fidlc");
        assert!(status.success());

        IR::load(ir_path)
    }
}
