// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context as _;
use anyhow::Result;
use argh::FromArgs;
use scrutiny_x as scrutiny;
use std::collections::HashSet;
use std::fs::read_dir;
use std::fs::write;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use tracing::debug;
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;

/// Arguments passed to the Scrutiny X smoke test intended to exercise Scrutiny X features against
/// a realistic assembled system.
#[derive(Debug, FromArgs, PartialEq)]
struct Args {
    /// path to depfile for build action management.
    #[argh(option)]
    pub depfile: Option<PathBuf>,

    /// path to stamp file for build action management.
    #[argh(option)]
    pub stamp: Option<PathBuf>,

    /// path to the directory containing the product bundle for the build.
    #[argh(option)]
    pub product_bundle: PathBuf,

    /// output debug logging.
    #[argh(switch)]
    pub debug: bool,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    if args.depfile.is_some() != args.stamp.is_some() {
        bail!("must specify both --depfile and --stamp, or neither");
    }

    if args.debug {
        tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(Level::TRACE)
            .with_span_events(FmtSpan::CLOSE)
            .init()
    }

    run_smoke_test(args)
}

#[tracing::instrument(level = "trace")]
fn run_smoke_test(args: Args) -> Result<()> {
    let Args { depfile, stamp, product_bundle, .. } = args;

    let product_bundle_path: Box<dyn scrutiny::Path> = Box::new(product_bundle.clone());
    let scrutiny = scrutiny::scrutiny(product_bundle_path, scrutiny::SystemVariant::Main)
        .map_err(|error| anyhow!("{}", error))?;

    output_data_sources(scrutiny.as_ref());
    output_blobs(scrutiny.as_ref());
    output_packages(scrutiny.as_ref());

    let bootfs = scrutiny.system().zbi().bootfs().expect("bootfs");
    output_bootfs_data(bootfs.as_ref());

    check_bootfs_blobs(scrutiny.as_ref());

    if let (Some(depfile), Some(stamp)) = (depfile, stamp) {
        let depfile = File::create(depfile).context("creating depfile")?;
        write_depfile(depfile, &product_bundle, &stamp).context("writing depfile")?;

        write(stamp, "Scrutiny X smoke test complete\n").context("writing stamp file")?;
    }

    Ok(())
}

#[tracing::instrument(level = "trace", skip_all)]
fn output_data_sources(scrutiny: &dyn scrutiny::Scrutiny) {
    for data_source in scrutiny.data_sources() {
        output_data_source(data_source, String::from(""));
    }
}

fn output_data_source(data_source: Box<dyn scrutiny::DataSource>, prefix: String) {
    debug!("{prefix}Data source: {data_source:?}", prefix = prefix, data_source = data_source);
    let prefix = if &prefix == "" {
        "  ↳ ".to_string()
    } else {
        let mut new_prefix = "  ".to_string();
        new_prefix.push_str(&prefix);
        new_prefix
    };
    for child in data_source.children() {
        output_data_source(child, prefix.clone())
    }
}

#[tracing::instrument(level = "trace", skip_all)]
fn output_blobs(scrutiny: &dyn scrutiny::Scrutiny) {
    for blob in scrutiny.blobs() {
        debug!("Blob: {:?}", blob.hash());
    }
}

#[tracing::instrument(level = "trace", skip_all)]
fn output_packages(scrutiny: &dyn scrutiny::Scrutiny) {
    for package_result in scrutiny.packages() {
        match package_result {
            Ok(package) => output_package(package.as_ref()),
            Err(error) => panic!("package error: {}", error),
        }
    }
}

fn output_package(package: &dyn scrutiny::Package) {
    debug!("Package: {:?}", package.hash());
    for (path, meta_blob) in package.meta_blobs() {
        debug!("  Meta blob at {:?}: {:?}", path.as_ref(), meta_blob.hash());
    }
    for (path, content_blob) in package.content_blobs() {
        debug!("  Content blob at {:?}: {:?}", path.as_ref(), content_blob.hash());
    }
    output_package_components(package);
}

fn output_package_components(package: &dyn scrutiny::Package) {
    for (path, _component) in package.components().expect("package components") {
        // TODO(111243): Exercise `Component` API once it is implemented.
        debug!("  Component at {:?}", path);
    }
}

fn output_bootfs_data(bootfs: &dyn scrutiny::Bootfs) {
    for (path, blob) in bootfs.content_blobs() {
        debug!("Bootfs file: {:?} {:?}", path.as_ref(), blob.hash());
    }

    for package in bootfs.packages().expect("bootfs packages") {
        output_bootfs_package(package.as_ref());
    }

    let additional_boot_configuration =
        bootfs.additional_boot_configuration().expect("additional boot configuration");
    output_additional_boot_configuration(additional_boot_configuration.as_ref());

    let component_manager_configuration =
        bootfs.component_manager_configuration().expect("component manager configuration");
    output_component_manager_configuration(component_manager_configuration.as_ref());
}

fn output_bootfs_package(package: &dyn scrutiny::Package) {
    debug!("Bootfs package: {:?}", package.hash());
    for (path, meta_blob) in package.meta_blobs() {
        debug!("  Meta blob at {:?}: {:?}", path.as_ref(), meta_blob.hash());
    }
    for (path, content_blob) in package.content_blobs() {
        debug!("  Content blob at {:?}: {:?}", path.as_ref(), content_blob.hash());
    }
}

fn output_additional_boot_configuration(configuration: &dyn scrutiny::AdditionalBootConfiguration) {
    debug!("Additional boot configuration:");
    for (key, value) in configuration.iter() {
        debug!("  {}: {}", key, value);
    }
}

fn output_component_manager_configuration(
    configuration: &dyn scrutiny::ComponentManagerConfiguration,
) {
    debug!("Component manager configuration:");
    debug!("  Component manager in debug mode?: {}", configuration.debug());
}

#[tracing::instrument(level = "trace", skip_all)]
fn check_bootfs_blobs(scrutiny: &dyn scrutiny::Scrutiny) {
    let bootfs_blob_hashes = scrutiny
        .system()
        .zbi()
        .bootfs()
        .expect("bootfs")
        .content_blobs()
        .map(|(_path, blob)| blob.hash())
        .collect::<HashSet<_>>();

    for blob in scrutiny.blobs() {
        if bootfs_blob_hashes.contains(&blob.hash()) {
            // Check that blob shipped in zbi/bootfs lists (at least) zbi as a data source.
            let data_sources = blob.data_sources().collect::<Vec<_>>();
            let zbi_bootfs_data_sources = data_sources
                .into_iter()
                .filter(|data_source| data_source.kind() == scrutiny_x::DataSourceKind::Zbi)
                .collect::<Vec<_>>();
            if zbi_bootfs_data_sources.len() != 1 {
                panic!(
                    "Expected exactly one zbi/bootfs data source for blob {:?}, but got {}",
                    blob.hash(),
                    zbi_bootfs_data_sources.len()
                );
            }
        }
    }
}

#[tracing::instrument(level = "trace", skip_all)]
fn write_depfile<W: Write, InputsPath: AsRef<Path>, OutputPath: AsRef<Path>>(
    mut depfile: W,
    inputs_path: InputsPath,
    output_path: OutputPath,
) -> Result<()> {
    depfile.write_all(path_bytes(output_path.as_ref())?)?;
    depfile.write_all(":".as_bytes())?;

    walk_for_depfile(&mut depfile, inputs_path)?;
    depfile.write_all("\n".as_bytes())?;

    Ok(())
}

fn walk_for_depfile<W: Write, P: AsRef<Path>>(depfile: &mut W, directory: P) -> Result<()> {
    for entry in read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            walk_for_depfile(depfile, &path)?;
        } else {
            depfile.write_all(" ".as_bytes())?;
            depfile.write_all(path_bytes(path.as_path())?)?;
        }
    }

    Ok(())
}

fn path_bytes(path: &Path) -> Result<&[u8]> {
    Ok(path.to_str().ok_or_else(|| anyhow!("invalid path: {:?}", path))?.as_bytes())
}
