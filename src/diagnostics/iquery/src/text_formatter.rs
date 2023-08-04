// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    diagnostics_data::{InspectData, InspectHandleName},
    diagnostics_hierarchy::{
        ArrayContent, DiagnosticsHierarchy, ExponentialHistogram, LinearHistogram, Property,
    },
    either::Either,
    nom::HexDisplay,
    num_traits::{Bounded, Zero},
    std::{
        cmp::PartialOrd,
        fmt,
        ops::{Add, AddAssign, Mul, MulAssign},
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

trait FromUsize {
    fn from_usize(n: usize) -> Self;
}

impl FromUsize for i64 {
    fn from_usize(n: usize) -> Self {
        i64::try_from(n).unwrap()
    }
}

impl FromUsize for u64 {
    fn from_usize(n: usize) -> Self {
        u64::try_from(n).unwrap()
    }
}

impl FromUsize for f64 {
    fn from_usize(n: usize) -> Self {
        u64::try_from(n).unwrap() as f64
    }
}

trait ExponentialBucketBound {
    fn bound(floor: Self, initial_step: Self, step_multiplier: Self, index: usize) -> Self;
}

impl ExponentialBucketBound for i64 {
    fn bound(floor: Self, initial_step: Self, step_multiplier: Self, index: usize) -> Self {
        floor + initial_step * i64::pow(step_multiplier, index as u32)
    }
}

impl ExponentialBucketBound for u64 {
    fn bound(floor: Self, initial_step: Self, step_multiplier: Self, index: usize) -> Self {
        floor + initial_step * u64::pow(step_multiplier, index as u32)
    }
}

impl ExponentialBucketBound for f64 {
    fn bound(floor: Self, initial_step: Self, step_multiplier: Self, index: usize) -> Self {
        floor + initial_step * f64::powi(step_multiplier, (index as u64) as i32)
    }
}

struct ExponentialBucketBoundsArgs<T> {
    floor: T,
    initial_step: T,
    step_multiplier: T,
    index: usize,
    size: usize,
}

fn exponential_bucket_bounds<T>(args: ExponentialBucketBoundsArgs<T>) -> (T, T)
where
    T: Bounded + Add<Output = T> + ExponentialBucketBound + Copy,
{
    let ExponentialBucketBoundsArgs { floor, initial_step, step_multiplier, index, size } = args;
    match index {
        0 => (T::min_value(), floor),
        1 if size == 2 => (floor, T::max_value()),
        1 => (floor, floor + initial_step),
        index if index == size - 1 => {
            (T::bound(floor, initial_step, step_multiplier, index - 2), T::max_value())
        }
        _ => (
            T::bound(floor, initial_step, step_multiplier, index - 2),
            T::bound(floor, initial_step, step_multiplier, index - 1),
        ),
    }
}

struct LinearBucketBoundsArgs<T> {
    floor: T,
    step: T,
    index: usize,
    size: usize,
}

fn linear_bucket_bounds<T>(args: LinearBucketBoundsArgs<T>) -> (T, T)
where
    T: Bounded + Add<Output = T> + Mul<Output = T> + FromUsize + Copy,
{
    let LinearBucketBoundsArgs { floor, step, index, size } = args;
    match index {
        0 => (T::min_value(), floor),
        index if index == size - 1 => (floor + step * T::from_usize(index - 1), T::max_value()),
        _ => (floor + step * T::from_usize(index - 1), floor + step * T::from_usize(index)),
    }
}

fn output_histogram<T, F, W>(
    w: &mut W,
    indent: &str,
    name: &str,
    histogram_type: &str,
    counts: &[T],
    indexes: &Option<Vec<usize>>,
    size: usize,
    bound_calculator: F,
) -> fmt::Result
where
    W: fmt::Write,
    T: NumberFormat + fmt::Display + PartialOrd + Zero,
    F: Fn(usize) -> (T, T),
{
    let value_indent = format!("{}{}", indent, " ".repeat(INDENT));
    write!(
        w,
        "{indent}{name}:\n\
                {value_indent}type = {histogram_type}\n\
                {value_indent}size = {size}\n\
                {value_indent}buckets = ["
    )?;
    let mut nonzero_counts = match indexes {
        None => Either::Left(counts.iter().enumerate().filter(|(_, count)| **count > T::zero())),
        Some(indexes) => Either::Right(
            indexes
                .iter()
                .zip(counts)
                .filter(|(_, count)| **count > T::zero())
                .map(|(u, t)| (*u, t)),
        ),
    }
    .peekable();
    while let Some((index, count)) = nonzero_counts.next() {
        let (low_bound, high_bound) = bound_calculator(index);
        write!(w, "[{},{})={}", low_bound.format(), high_bound.format(), count)?;
        if nonzero_counts.peek().is_some() {
            write!(w, ", ")?;
        }
    }
    write!(w, "]\n")
}

fn output_array<T, W>(
    w: &mut W,
    value_indent: &str,
    name: &str,
    array: &ArrayContent<T>,
) -> fmt::Result
where
    W: fmt::Write,
    T: AddAssign
        + MulAssign
        + Copy
        + Add<Output = T>
        + fmt::Display
        + NumberFormat
        + Bounded
        + Mul<Output = T>
        + ExponentialBucketBound
        + FromUsize
        + PartialOrd
        + Zero,
{
    match array {
        ArrayContent::Values(values) => {
            write!(w, "{value_indent}{name} = [")?;
            for (i, value) in values.iter().enumerate() {
                write!(w, "{value}")?;
                if i < values.len() - 1 {
                    w.write_str(", ")?;
                }
            }
            writeln!(w, "]")
        }
        ArrayContent::LinearHistogram(LinearHistogram { floor, step, counts, indexes, size }) => {
            let bucket_bounder = |index| {
                linear_bucket_bounds::<T>(LinearBucketBoundsArgs {
                    floor: *floor,
                    step: *step,
                    index,
                    size: *size,
                })
            };
            output_histogram(
                w,
                value_indent,
                name,
                "linear",
                counts,
                indexes,
                *size,
                bucket_bounder,
            )
        }
        ArrayContent::ExponentialHistogram(ExponentialHistogram {
            floor,
            initial_step,
            step_multiplier,
            counts,
            indexes,
            size,
        }) => {
            let bucket_bounder = |index| {
                exponential_bucket_bounds::<T>(ExponentialBucketBoundsArgs {
                    floor: *floor,
                    initial_step: *initial_step,
                    step_multiplier: *step_multiplier,
                    index,
                    size: *size,
                })
            };
            output_histogram(
                w,
                value_indent,
                name,
                "exponential",
                counts,
                indexes,
                *size,
                bucket_bounder,
            )
        }
    }
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

impl NumberFormat for usize {
    fn format(&self) -> String {
        match *self {
            std::usize::MAX => "<max>".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 0, size: 4 }, (i64::MIN, -10))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 1, size: 4 }, (-10, -8))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 2, size: 4 }, (-8, -6))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 3, size: 4 }, (-6, i64::MAX))]
    #[fuchsia::test]
    fn test_linear_bucket_bounds_i64(args: LinearBucketBoundsArgs<i64>, bounds: (i64, i64)) {
        // floor, step, index, size
        assert_eq!(linear_bucket_bounds(args), bounds);
    }

    // u64 min is 0, so the underflow bucket may be weird
    #[test_case(LinearBucketBoundsArgs { floor: 0, step: 2, index: 0, size: 4 }, (0, 0))]
    #[test_case(LinearBucketBoundsArgs { floor: 0, step: 2, index: 1, size: 4 }, (0, 2))]
    #[test_case(LinearBucketBoundsArgs { floor: 0, step: 2, index: 2, size: 4 }, (2, 4))]
    #[test_case(LinearBucketBoundsArgs { floor: 0, step: 2, index: 3, size: 4 }, (4, u64::MAX))]
    #[test_case(LinearBucketBoundsArgs { floor: 1, step: 2, index: 0, size: 4 }, (0, 1))]
    #[test_case(LinearBucketBoundsArgs { floor: 1, step: 2, index: 1, size: 4 }, (1, 3))]
    #[test_case(LinearBucketBoundsArgs { floor: 1, step: 2, index: 2, size: 4 }, (3, 5))]
    #[test_case(LinearBucketBoundsArgs { floor: 1, step: 2, index: 3, size: 4 }, (5, u64::MAX))]
    #[fuchsia::test]
    fn test_linear_bucket_bounds_u64(args: LinearBucketBoundsArgs<u64>, bounds: (u64, u64)) {
        assert_eq!(linear_bucket_bounds(args), bounds);
    }

    #[test_case(
        LinearBucketBoundsArgs { floor: -0.5, step: 0.5, index: 0, size: 4 },
        (f64::MIN, -0.5)
    )]
    #[test_case(LinearBucketBoundsArgs { floor: -0.5, step: 0.5, index: 1, size: 4 }, (-0.5, 0.0))]
    #[test_case(LinearBucketBoundsArgs { floor: -0.5, step: 0.5, index: 2, size: 4 }, (0.0, 0.5))]
    #[test_case(
        LinearBucketBoundsArgs { floor: -0.5, step: 0.5, index: 3, size: 4 },
        (0.5, f64::MAX)
    )]
    #[fuchsia::test]
    fn test_linear_bucket_bounds_f64(args: LinearBucketBoundsArgs<f64>, bounds: (f64, f64)) {
        assert_eq!(linear_bucket_bounds(args), bounds);
    }

    // Check logic for few-bucket histograms. 2-bucket shouldn't occur, but let's treat it
    // well anyway. (Using i64)
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 0, size: 3 }, (i64::MIN, -10))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 1, size: 3 }, (-10, -8))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 2, size: 3 }, (-8, i64::MAX))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 0, size: 2 }, (i64::MIN, -10))]
    #[test_case(LinearBucketBoundsArgs { floor: -10, step: 2, index: 1, size: 2 }, (-10, i64::MAX))]
    #[fuchsia::test]
    fn test_linear_small_bucket_bounds(args: LinearBucketBoundsArgs<i64>, bounds: (i64, i64)) {
        // floor, step, index, size
        assert_eq!(linear_bucket_bounds(args), bounds);
    }

    // Test cases for i64 exponential
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 0,
            size: 5 },
        (i64::MIN, -10)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 1,
            size: 5 },
        (-10, -8)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 2,
            size: 5
        },
        (-8, -4)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 3,
            size: 5
        },
        (-4, 8)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 4,
            size: 5
        },
        (8, i64::MAX)
    )]
    #[fuchsia::test]
    fn test_exponential_bucket_bounds_i64(
        args: ExponentialBucketBoundsArgs<i64>,
        bounds: (i64, i64),
    ) {
        assert_eq!(exponential_bucket_bounds(args), bounds);
    }

    // Test cases for u64 exponential
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 0,
            initial_step: 2,
            step_multiplier: 3,
            index: 0,
            size: 5
        },
        (0, 0)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 0,
            initial_step: 2,
            step_multiplier: 3,
            index: 1,
            size: 5
        },
        (0, 2)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 0,
            initial_step: 2,
            step_multiplier: 3,
            index: 2,
            size: 5
        },
        (2, 6)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 0,
            initial_step: 2,
            step_multiplier: 3,
            index: 3,
            size: 5
        },
        (6, 18)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
        floor: 0,
        initial_step: 2,
        step_multiplier: 3,
        index: 4,
        size: 5 },
        (18, u64::MAX)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 1,
            initial_step: 2,
            step_multiplier: 3,
            index: 0,
            size: 5
        },
        (0, 1)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 1,
            initial_step: 2,
            step_multiplier: 3,
            index: 1,
            size: 5
        },
        (1, 3)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 1,
            initial_step: 2,
            step_multiplier: 3,
            index: 2,
            size: 5
        },
        (3, 7)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 1,
            initial_step: 2,
            step_multiplier: 3,
            index: 3,
            size: 5
        },
        (7, 19)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: 1,
            initial_step: 2,
            step_multiplier: 3,
            index: 4,
            size: 5
        },
        (19, u64::MAX)
    )]
    #[fuchsia::test]
    fn test_exponential_bucket_bounds_u64(
        args: ExponentialBucketBoundsArgs<u64>,
        bounds: (u64, u64),
    ) {
        assert_eq!(exponential_bucket_bounds(args), bounds);
    }

    // Test cases for f64 exponential
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -0.5,
            initial_step: 0.5,
            step_multiplier: 3.0,
            index: 0,
            size: 5 },
        (f64::MIN, -0.5)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -0.5,
            initial_step: 0.5,
            step_multiplier: 3.0,
            index: 1,
            size: 5 },
        (-0.5, 0.0)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -0.5,
            initial_step: 0.5,
            step_multiplier: 3.0,
            index: 2,
            size: 5 },
        (0.0, 1.0)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -0.5,
            initial_step: 0.5,
            step_multiplier: 3.0,
            index: 3,
            size: 5 },
        (1.0, 4.0)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -0.5,
            initial_step: 0.5,
            step_multiplier: 3.0,
            index: 4,
            size: 5 },
            (4.0, f64::MAX)
        )]
    #[fuchsia::test]
    fn test_exponential_bucket_bounds_f64(
        args: ExponentialBucketBoundsArgs<f64>,
        bounds: (f64, f64),
    ) {
        assert_eq!(exponential_bucket_bounds(args), bounds);
    }

    // Check logic for few-bucket histograms. 2-bucket shouldn't occur, but let's treat it
    // well anyway. (Using i64)
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 0,
            size: 4
        },
        (i64::MIN, -10)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 1,
            size: 4
        },
        (-10, -8)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 2,
            size: 4
        },
        (-8, -4)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 3,
            size: 4
        },
        (-4, i64::MAX)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 0,
            size: 3
        },
        (i64::MIN, -10)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 1,
            size: 3
        },
        (-10, -8)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 2,
            size: 3
        },
        (-8, i64::MAX)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 0,
            size: 2
        },
        (i64::MIN, -10)
    )]
    #[test_case(
        ExponentialBucketBoundsArgs {
            floor: -10,
            initial_step: 2,
            step_multiplier: 3,
            index: 1,
            size: 2
        },
        (-10, i64::MAX)
    )]
    #[fuchsia::test]
    fn test_exponential_small_bucket_bounds(
        args: ExponentialBucketBoundsArgs<i64>,
        bounds: (i64, i64),
    ) {
        assert_eq!(exponential_bucket_bounds(args), bounds);
    }
}
