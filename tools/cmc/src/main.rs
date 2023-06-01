// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `cmc` is the Component Manifest Compiler.

use anyhow::{ensure, Error};
use cml::features;
pub use cml::{self, error, one_or_many, translate, Document};
use reference_doc::MarkdownReferenceDocGenerator;
use std::fs;
use std::path::{Path, PathBuf};
use structopt::StructOpt;

#[allow(unused)] // A test-only macro is defined on all builds.
mod compile;
mod format;
mod include;
mod merge;
mod opts;
mod reference;
mod util;

fn main() -> Result<(), Error> {
    run_cmc()
}

fn path_exists(path: &Path) -> Result<(), Error> {
    ensure!(path.exists(), "{:?} does not exist", path);
    Ok(())
}

fn optional_path_exists(optional_path: Option<&PathBuf>) -> Result<(), Error> {
    if let Some(path) = optional_path.as_ref() {
        ensure!(path.exists(), "{:?} does not exist", path);
    }
    Ok(())
}

fn run_cmc() -> Result<(), Error> {
    let opt = opts::Opt::from_args();
    match opt.cmd {
        // TODO(fxbug.dev/69367): Remove `cmc validate`.
        opts::Commands::Validate { files, must_offer_protocol, must_use_protocol } => {
            if files.is_empty() {
                return Err(error::Error::invalid_args("No files provided").into());
            }

            for file in files {
                let file = file.as_ref();
                cml::compile(
                    &util::read_cml(file)?,
                    cml::CompileOptions::new().file(&file).protocol_requirements(
                        cml::ProtocolRequirements {
                            must_offer: &must_offer_protocol,
                            must_use: &must_use_protocol,
                        },
                    ),
                )?;
            }
        }
        opts::Commands::ValidateReferences { component_manifest, package_manifest, context } => {
            reference::validate(&component_manifest, &package_manifest, context.as_ref())?
        }
        opts::Commands::Merge { files, output, fromfile, depfile } => {
            merge::merge(files, output, fromfile, depfile)?
        }
        opts::Commands::Include { file, output, depfile, includepath, includeroot, validate } => {
            path_exists(&file)?;
            include::merge_includes(
                &file,
                output.as_ref(),
                depfile.as_ref(),
                &includepath,
                &includeroot,
                validate,
            )?
        }
        opts::Commands::CheckIncludes {
            file,
            expected_includes,
            fromfile,
            depfile,
            includepath,
            includeroot,
        } => {
            path_exists(&file)?;
            optional_path_exists(fromfile.as_ref())?;
            include::check_includes(
                &file,
                expected_includes,
                fromfile.as_ref(),
                depfile.as_ref(),
                opt.stamp.as_ref(),
                &includepath,
                &includeroot,
            )?
        }
        opts::Commands::Format { file, pretty, cml, inplace, mut output } => {
            // TODO(fxbug.dev/109014): stop accepting these flags.
            let _pretty = pretty;
            let _cml = cml;

            path_exists(&file)?;
            if inplace {
                output = Some(file.clone());
            }
            format::format(&file, output)?;
        }
        opts::Commands::Compile {
            file,
            output,
            depfile,
            includepath,
            includeroot,
            config_package_path,
            features,
            experimental_force_runner,
            must_offer_protocol,
            must_use_protocol,
        } => {
            path_exists(&file)?;
            compile::compile(
                &file,
                &output.unwrap(),
                depfile,
                &includepath,
                &includeroot,
                config_package_path.as_ref().map(String::as_str),
                &features.into(),
                &experimental_force_runner,
                cml::ProtocolRequirements {
                    must_offer: &must_offer_protocol,
                    must_use: &must_use_protocol,
                },
            )?
        }
        opts::Commands::PrintReferenceDocs { output } => {
            let docs = Document::get_reference_doc_markdown();
            match &output {
                None => println!("{}", docs),
                Some(path) => fs::write(path, docs)?,
            }
        }
    }
    if let Some(stamp_path) = opt.stamp {
        stamp(stamp_path)?;
    }
    Ok(())
}

fn stamp(stamp_path: PathBuf) -> Result<(), Error> {
    fs::File::create(stamp_path)?;
    Ok(())
}
