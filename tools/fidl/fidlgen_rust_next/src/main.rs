// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Next-generation FIDL Rust bindings generator

#![deny(
    future_incompatible,
    missing_docs,
    nonstandard_style,
    unused,
    warnings,
    clippy::all,
    clippy::alloc_instead_of_core,
    clippy::missing_safety_doc,
    clippy::std_instead_of_core,
    clippy::undocumented_unsafe_blocks,
    rustdoc::broken_intra_doc_links,
    rustdoc::missing_crate_level_docs
)]
#![forbid(unsafe_op_in_unsafe_fn)]

mod config;
mod de;
mod id;
mod ir;
mod templates;

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::Command;

use argh::FromArgs;
use askama::Template;

use self::config::{Config, ResourceBindings};
use self::ir::Schema;
use self::templates::Context;

/// Generate Rust bindings from FIDL IR
#[derive(FromArgs)]
struct Fidlgen {
    /// source JSON IR file path
    #[argh(option)]
    json: PathBuf,
    /// output file path
    #[argh(option)]
    output_filename: PathBuf,
    /// rustfmt binary path
    #[argh(option)]
    rustfmt: PathBuf,
    /// rustfmt configuration file path
    #[argh(option)]
    rustfmt_config: PathBuf,
}

fn main() {
    let args = argh::from_env::<Fidlgen>();

    let file = File::open(&args.json).expect("failed to open source JSON IR file");
    let schema = serde_json::from_reader::<_, Schema>(BufReader::new(file))
        .expect("failed to parse source JSON IR");

    let config = Config { emit_debug_impls: true, resource_bindings: ResourceBindings::default() };
    let context = Context::new(schema, config);
    let out = File::create(&args.output_filename).expect("failed to create output file");
    context.write_into(&mut BufWriter::new(out)).expect("failed to emit FIDL bindings");

    Command::new(args.rustfmt)
        .arg("--config-path")
        .arg(&args.rustfmt_config)
        .arg(&args.output_filename)
        .status()
        .unwrap();
}
