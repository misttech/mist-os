// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    diagnostics_data::{InspectData, InspectHandleName},
    diagnostics_hierarchy::{ArrayContent, DiagnosticsHierarchy, Property},
    nom::HexDisplay,
    num_traits::Bounded,
    std::{
        fmt,
        ops::{Add, AddAssign, MulAssign},
    },
};

const INDENT: usize = 2;
const HEX_DISPLAY_CHUNK_SIZE: usize = 16;

pub fn format<W>(w: &mut W, path: &str, diagnostics_hierarchy: DiagnosticsHierarchy) -> fmt::Result
where
    W: fmt::Write,
{
    writeln!(w, "{}:", path)?;
    output_hierarchy(w, &diagnostics_hierarchy, 1)
}

pub fn output_schema<W>(w: &mut W, schema: &InspectData) -> fmt::Result
where
    W: fmt::Write,
{
    writeln!(w, "{}:", schema.moniker)?;
    writeln!(w, "  metadata:")?;
    if let Some(errors) = &schema.metadata.errors {
        writeln!(w, "    errors = {}", errors.join(", "))?;
    }

    if let Some(name) = &schema.metadata.name {
        match name {
            InspectHandleName::Filename(f) => {
                writeln!(w, "    filename = {}", f)?;
            }
            InspectHandleName::Name(n) => {
                writeln!(w, "    name = {}", n)?;
            }
        }
    }

    write!(w, "    component_url = ")?;
    match &schema.metadata.component_url {
        Some(url) => writeln!(w, "{}", url)?,
        None => writeln!(w, "null")?,
    }
    writeln!(w, "    timestamp = {}", schema.metadata.timestamp)?;

    match &schema.payload {
        Some(hierarchy) => {
            writeln!(w, "  payload:")?;
            output_hierarchy(w, &hierarchy, 2)
        }
        None => writeln!(w, "  payload: null"),
    }
}

fn output_hierarchy<W>(
    w: &mut W,
    diagnostics_hierarchy: &DiagnosticsHierarchy,
    indent: usize,
) -> fmt::Result
where
    W: fmt::Write,
{
    let name_indent = " ".repeat(INDENT * indent);
    let value_indent = " ".repeat(INDENT * (indent + 1));

    writeln!(w, "{}{}:", name_indent, diagnostics_hierarchy.name)?;

    for property in &diagnostics_hierarchy.properties {
        match property {
            Property::String(name, value) => writeln!(w, "{}{} = {}", value_indent, name, value)?,
            Property::Int(name, value) => writeln!(w, "{}{} = {}", value_indent, name, value)?,
            Property::Uint(name, value) => writeln!(w, "{}{} = {}", value_indent, name, value)?,
            Property::Double(name, value) => {
                writeln!(w, "{}{} = {:.6}", value_indent, name, value)?
            }
            Property::Bytes(name, array) => {
                let byte_str = array.to_hex(HEX_DISPLAY_CHUNK_SIZE);
                writeln!(w, "{}{} = Binary:\n{}", value_indent, name, byte_str.trim())?;
            }
            Property::Bool(name, value) => writeln!(w, "{}{} = {}", value_indent, name, value)?,
            Property::IntArray(name, array) => output_array(w, &value_indent, &name, &array)?,
            Property::UintArray(name, array) => output_array(w, &value_indent, &name, &array)?,
            Property::DoubleArray(name, array) => output_array(w, &value_indent, &name, &array)?,
            Property::StringList(name, list) => {
                writeln!(w, "{}{} = {:?}", value_indent, name, list)?;
            }
        }
    }

    for child in &diagnostics_hierarchy.children {
        output_hierarchy(w, child, indent + 1)?;
    }
    Ok(())
}

fn output_array<T, W>(
    w: &mut W,
    value_indent: &str,
    name: &str,
    array: &ArrayContent<T>,
) -> fmt::Result
where
    W: fmt::Write,
    T: AddAssign + MulAssign + Copy + Add<Output = T> + fmt::Display + NumberFormat + Bounded,
{
    write!(w, "{}{} = [", value_indent, name)?;
    match array {
        ArrayContent::Values(values) => {
            for (i, value) in values.iter().enumerate() {
                write!(w, "{}", value)?;
                if i < values.len() - 1 {
                    write!(w, ", ")?;
                }
            }
        }
        ArrayContent::Buckets(buckets) => {
            for (i, bucket) in buckets.iter().enumerate() {
                write!(
                    w,
                    "[{},{})={}",
                    bucket.floor.format(),
                    bucket.ceiling.format(),
                    bucket.count.format()
                )?;
                if i < buckets.len() - 1 {
                    write!(w, ", ")?;
                }
            }
        }
    };
    writeln!(w, "]")
}

trait NumberFormat {
    fn format(&self) -> String;
}

impl NumberFormat for i64 {
    fn format(&self) -> String {
        match *self {
            std::i64::MAX => "<max>".to_string(),
            std::i64::MIN => "<min>".to_string(),
            x => format!("{}", x),
        }
    }
}

impl NumberFormat for u64 {
    fn format(&self) -> String {
        match *self {
            std::u64::MAX => "<max>".to_string(),
            x => format!("{}", x),
        }
    }
}

impl NumberFormat for f64 {
    fn format(&self) -> String {
        if *self == std::f64::MAX || *self == std::f64::INFINITY {
            "inf".to_string()
        } else if *self == std::f64::MIN || *self == std::f64::NEG_INFINITY {
            "-inf".to_string()
        } else {
            format!("{}", self)
        }
    }
}
