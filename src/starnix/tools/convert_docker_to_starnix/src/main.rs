// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Result};
use fuchsia_async as fasync;
use fuchsia_pkg::PackageBuilder;
use std::{fs::File, path::Path};
use tar_img_extract::docker_archive::{DockerArchive, DockerArchiveArchitecture};
use tar_img_extract::{tar_img_extract_docker_archive, tar_img_extract_tarball, InputFormat};
use tempfile::TempDir;

mod args;
use args::{Command, ContainerArchitecture};

mod component_manifest;
use component_manifest::{compile_container_manifest, compile_exec_manifest};

mod default_init;
use default_init::{DEFAULT_INIT_ARM64, DEFAULT_INIT_X64};

#[fasync::run_singlethreaded]
async fn main() -> Result<()> {
    let cmd: Command = argh::from_env();

    // Deduce the output package name from the output path.
    let package_name = Path::new(&cmd.output_file)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_suffix(".far")
        .context("the output filename must end with .far")?;

    // Create a new Fuchsia package.
    let mut package_builder = PackageBuilder::new(package_name);
    let temp_dir = TempDir::new()?;
    let input_file = File::open(&cmd.input_path)?;
    // Extract the container TAR image and add all files to package builder.
    let extract_dir = temp_dir.path().join("extr");

    let (file_manifest, architecture) = match cmd.input_format {
        InputFormat::Tarball => {
            let manifest = tar_img_extract_tarball(&input_file, &extract_dir)?;
            // The target architecture must have been explicitly passed from command line arguments.
            (manifest, cmd.arch.context("missing --arch option")?)
        }
        InputFormat::DockerArchive => {
            // Load it as a Docker archive.
            let input_archive = DockerArchive::open(input_file)?;

            // Add a manifest that runs the container's default command.
            package_builder.add_contents_to_far(
                "meta/default_command.cm",
                compile_exec_manifest(&input_archive.default_command(), &input_archive.environ()),
                temp_dir.path(),
            )?;

            // Read the architecture from the archive's metadata.
            let arch_from_metadata = match input_archive.architecture() {
                DockerArchiveArchitecture::Amd64 => ContainerArchitecture::X64,
                DockerArchiveArchitecture::Arm64 => ContainerArchitecture::Arm64,
            };
            if cmd.arch.is_some_and(|given| given != arch_from_metadata) {
                bail!("The --arch value does not match the contents of the input archive");
            }

            let manifest = tar_img_extract_docker_archive(&input_archive, &extract_dir)?;
            (manifest, arch_from_metadata)
        }
    };

    // Add files to package.
    for (inode_str, file_path) in &file_manifest {
        package_builder
            .add_file_as_blob(format!("data/rootfs/{}", inode_str), file_path)
            .expect("Failed to add file to package builder");
    }

    // Add the default_init executable corresponding to the target architecture.
    let default_init = match architecture {
        ContainerArchitecture::Arm64 => DEFAULT_INIT_ARM64,
        ContainerArchitecture::X64 => DEFAULT_INIT_X64,
    };
    package_builder.add_contents_as_blob("data/init", default_init, temp_dir.path())?;

    // Add the container's manifest.
    package_builder.add_contents_to_far(
        "meta/container.cm",
        compile_container_manifest(
            package_name,
            &[
                "/:remote_bundle:data/rootfs",
                "/dev:devtmpfs",
                "/dev/pts:devpts",
                "/dev/shm:tmpfs",
                "/proc:proc",
                "/sys:sysfs",
                "/tmp:tmpfs",
            ],
            &cmd.features,
        ),
        temp_dir.path(),
    )?;

    // Write the archive into the output file.
    let gendir = temp_dir.path().join("gen");
    let manifest = package_builder.build(gendir, temp_dir.path().join("meta.far"))?;
    manifest.archive(temp_dir.path(), File::create(cmd.output_file)?).await?;
    Ok(())
}
