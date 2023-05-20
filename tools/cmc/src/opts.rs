// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::features::Feature;
use std::path::PathBuf;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
/// Tool for assembly, compilation, and validation of component manifests.
pub struct Opt {
    #[structopt(short = "s", long = "stamp", parse(from_os_str))]
    /// Stamp this file on success
    pub stamp: Option<PathBuf>,

    #[structopt(subcommand)]
    pub cmd: Commands,
}

#[derive(StructOpt, Debug)]
pub enum Commands {
    #[structopt(name = "validate")]
    /// validate that one or more cml files are valid
    Validate {
        #[structopt(name = "FILE", parse(from_os_str))]
        /// files to process
        files: Vec<PathBuf>,

        #[structopt(long = "must-offer-protocol")]
        /// verifies that all children and collections are offered the given protocols
        ///
        /// If specified, for each protocol named, cmc will require that all children
        /// or collections in `files` have been offered the given protocol. This can be
        /// used to help find missing offers of capabilities like `fuchsia.logger.LogSink`
        must_offer_protocol: Vec<String>,

        #[structopt(long = "must-use-protocol")]
        /// verifies that components use the given protocols
        ///
        /// If specified, for each protocol named, cmc will require that each component in
        /// `files` uses the given protocol. This can be used to help find missing usages
        /// of capabilities like `fuchsia.logger.LogSink`
        must_use_protocol: Vec<String>,
    },

    #[structopt(name = "validate-references")]
    /// validate a component manifest against package manifest.
    ValidateReferences {
        #[structopt(
            name = "Component Manifest",
            short = "c",
            long = "component-manifest",
            parse(from_os_str)
        )]
        component_manifest: PathBuf,

        #[structopt(
            name = "Package Manifest",
            short = "p",
            long = "package-manifest",
            parse(from_os_str)
        )]
        package_manifest: PathBuf,

        #[structopt(
            name = "Free text label, for instance as context for errors printed",
            short = "e",
            long = "context"
        )]
        context: Option<String>,
    },

    #[structopt(name = "merge")]
    /// merge the listed manifest files. Does NOT validate the resulting manifest.
    ///
    /// The semantics for merging are the same ones used for `include`:
    /// https://fuchsia.dev/reference/cml#include
    Merge {
        #[structopt(name = "FILE", parse(from_os_str))]
        /// files to process
        ///
        /// If any file contains an array at its root, every object in the array
        /// will be merged into the final object.
        files: Vec<PathBuf>,

        #[structopt(short = "o", long = "output", parse(from_os_str))]
        /// file to write the merged results to, will print to stdout if not provided
        output: Option<PathBuf>,

        #[structopt(short = "f", long = "fromfile", parse(from_os_str))]
        /// response file for files to process
        ///
        /// If specified, additional files to merge will be read from the path provided.
        /// The input format is delimited by newlines.
        fromfile: Option<PathBuf>,

        #[structopt(short = "d", long = "depfile", parse(from_os_str))]
        /// depfile for includes
        ///
        /// If specified, include paths will be listed here, delimited by newlines.
        depfile: Option<PathBuf>,
    },

    #[structopt(name = "include")]
    /// recursively process contents from includes, and optionally validate the result
    Include {
        #[structopt(name = "FILE", parse(from_os_str))]
        /// file to process
        file: PathBuf,

        #[structopt(short = "o", long = "output", parse(from_os_str))]
        /// file to write the merged results to, will print to stdout if not provided
        output: Option<PathBuf>,

        #[structopt(short = "d", long = "depfile", parse(from_os_str))]
        /// depfile for includes
        ///
        /// If specified, include paths will be listed here, delimited by newlines.
        depfile: Option<PathBuf>,

        #[structopt(short = "p", long = "includepath", parse(from_os_str), default_value = "")]
        /// base paths for resolving includes
        includepath: Vec<PathBuf>,

        #[structopt(short = "r", long = "includeroot", parse(from_os_str), default_value = "")]
        /// base path for resolving include paths that start with "//"
        includeroot: PathBuf,

        #[structopt(long = "validate", parse(try_from_str), default_value = "true")]
        /// validate the result
        validate: bool,
    },

    #[structopt(name = "check-includes")]
    /// check if given includes are present in a given component manifest
    CheckIncludes {
        #[structopt(name = "FILE", parse(from_os_str))]
        /// file to process
        file: PathBuf,

        #[structopt(name = "expect")]
        expected_includes: Vec<String>,

        #[structopt(short = "f", long = "fromfile", parse(from_os_str))]
        /// response file for includes to expect
        ///
        /// If specified, additional includes to expect will be read from the path provided.
        /// The input format is delimited by newlines.
        fromfile: Option<PathBuf>,

        #[structopt(short = "d", long = "depfile", parse(from_os_str))]
        /// depfile for includes
        ///
        /// If specified, include paths will be listed here, delimited by newlines.
        depfile: Option<PathBuf>,

        #[structopt(short = "p", long = "includepath", parse(from_os_str), default_value = "")]
        /// base paths for resolving includes
        includepath: Vec<PathBuf>,

        #[structopt(short = "r", long = "includeroot", parse(from_os_str), default_value = "")]
        /// base path for resolving include paths that start with "//"
        includeroot: PathBuf,
    },

    #[structopt(name = "format")]
    /// format a json file
    Format {
        #[structopt(name = "FILE", parse(from_os_str))]
        /// file to format
        file: PathBuf,

        #[structopt(short = "i", long = "in-place")]
        /// replace the input file with the formatted output (implies `--output <inputfile>`)
        inplace: bool,

        #[structopt(short = "o", long = "output", parse(from_os_str))]
        /// file to write the formatted results to, will print to stdout if not provided
        output: Option<PathBuf>,
    },

    #[structopt(name = "compile")]
    /// compile a CML file
    Compile {
        #[structopt(name = "FILE", parse(from_os_str))]
        /// file to format
        file: PathBuf,

        #[structopt(short = "o", long = "output", parse(from_os_str))]
        /// file to write the formatted results to, will print to stdout if not provided
        output: Option<PathBuf>,

        #[structopt(short = "d", long = "depfile", parse(from_os_str))]
        /// depfile for includes
        ///
        /// If specified, include paths will be listed here, delimited by newlines.
        depfile: Option<PathBuf>,

        #[structopt(short = "p", long = "includepath", parse(from_os_str), default_value = "")]
        /// base paths for resolving includes
        includepath: Vec<PathBuf>,

        #[structopt(short = "r", long = "includeroot", parse(from_os_str), default_value = "")]
        /// base path for resolving include paths that start with "//"
        includeroot: PathBuf,

        #[structopt(long = "config-package-path")]
        /// path within the component's package at which its configuration will be available
        config_package_path: Option<String>,

        #[structopt(short = "f", long = "features")]
        /// The set of non-standard features to compile with.
        /// Only applies to CML files.
        features: Vec<Feature>,

        #[structopt(long = "experimental-force-runner")]
        /// override runner to this value in resulting CML
        ///
        /// If specified, the program.runner field will be set to this value. This option is
        /// EXPERIMENTAL and subject to removal without warning.
        experimental_force_runner: Option<String>,

        #[structopt(long = "must-offer-protocol")]
        /// protocols to verify that all children and collections are offered
        ///
        /// If specified, for each offer named, cmc will require that all children or collections
        /// in `files` have been offered a capability named for the offer specified.  This can be
        /// used to help find missing offers of important capabilities, like fuchsia.logger.LogSink
        must_offer_protocol: Vec<String>,

        #[structopt(long = "must-use-protocol")]
        /// protocols to verify that all children and collections are used
        ///
        /// If specified, for each offer named, cmc will require that the offer is in a use block.
        /// This can be used to help find missing usages of important capabilities, like
        /// fuchsia.logger.LogSink
        must_use_protocol: Vec<String>,
    },

    #[structopt(name = "print-cml-reference")]
    /// print generated .cml reference documentation
    PrintReferenceDocs {
        #[structopt(name = "file path", short = "o", long = "output", parse(from_os_str))]
        /// If provided, will output generated reference documentation to a text
        /// file at the file path provided.
        output: Option<PathBuf>,
    },
}
