// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Result};
use ext4_metadata::ROOT_INODE_NUM;
use fuchsia_async as fasync;
use fuchsia_pkg::{PackageBuilder, PathToStringExt};
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use tempfile::TempDir;

mod args;
use args::{Command, ContainerArchitecture, InputFormat};

mod component_manifest;
use component_manifest::{compile_container_manifest, compile_exec_manifest};

mod default_init;
use default_init::{DEFAULT_INIT_ARM64, DEFAULT_INIT_X64};

mod docker_archive;
use docker_archive::{DockerArchive, DockerArchiveArchitecture};

mod layered_image;
use layered_image::{DirectoryVisitor, LayeredImage};

const S_IFDIR: u16 = 16384;
const S_IFREG: u16 = 32768;
const S_IFLNK: u16 = 40960;

/// A directory visitor that fills an ext4 Metadata object and registers its blobs into a package
/// builder.
struct PopulateBundleVisitor {
    package_builder: PackageBuilder,
    ext4_metadata: ext4_metadata::Metadata,
    visited_inode_nums: HashSet<u64>,
    name_stack: Vec<String>,
}

impl PopulateBundleVisitor {
    fn add_child_at_current_name_stack(&mut self, inode_num: u64) {
        let path: Vec<&str> = self.name_stack.iter().map(String::as_str).collect();
        self.ext4_metadata.add_child(&path, inode_num);
    }
}

impl DirectoryVisitor for PopulateBundleVisitor {
    fn visit_file(&mut self, name: &[u8], file: &layered_image::File) {
        let inode_num = file.metadata().inode_num();
        let is_first_visit = self.visited_inode_nums.insert(inode_num);

        if is_first_visit {
            self.ext4_metadata.insert_file(
                inode_num,
                file.metadata().mode() | S_IFREG,
                file.metadata().uid(),
                file.metadata().gid(),
                file.metadata().extended_attributes(),
            );

            self.package_builder
                .add_file_as_blob(
                    format!("data/rootfs/{}", inode_num),
                    file.data_file_path().path_to_string().unwrap(),
                )
                .expect("Failed to add file to package builder");
        }

        let name = String::from_utf8(name.to_vec()).expect("Invalid utf8 in name");
        self.name_stack.push(name);
        self.add_child_at_current_name_stack(inode_num);
        self.name_stack.pop();
    }

    fn visit_symlink(&mut self, name: &[u8], symlink: &layered_image::Symlink) {
        let inode_num = symlink.metadata().inode_num();
        let is_first_visit = self.visited_inode_nums.insert(inode_num);

        if is_first_visit {
            self.ext4_metadata.insert_symlink(
                inode_num,
                String::from_utf8(symlink.target().to_vec()).expect("Invalid utf8 in target"),
                symlink.metadata().mode() | S_IFLNK,
                symlink.metadata().uid(),
                symlink.metadata().gid(),
                symlink.metadata().extended_attributes(),
            );
        }

        let name = String::from_utf8(name.to_vec()).expect("Invalid utf8 in name");
        self.name_stack.push(name);
        self.add_child_at_current_name_stack(inode_num);
        self.name_stack.pop();
    }

    fn visit_directory(&mut self, name: &[u8], directory: &layered_image::Directory) {
        let inode_num = directory.metadata().inode_num();
        let is_first_visit = self.visited_inode_nums.insert(inode_num);

        if is_first_visit {
            self.ext4_metadata.insert_directory(
                inode_num,
                directory.metadata().mode() | S_IFDIR,
                directory.metadata().uid(),
                directory.metadata().gid(),
                directory.metadata().extended_attributes(),
            );
        }

        let name = String::from_utf8(name.to_vec()).expect("Invalid utf8 in name");
        self.name_stack.push(name);
        self.add_child_at_current_name_stack(inode_num);
        if is_first_visit {
            directory.visit(self); // recurse into directory
        }
        self.name_stack.pop();
    }
}

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

    // Create an empty LayeredImage and fill it with files from input archive.
    let mut image = LayeredImage::new(&temp_dir.path().join("extr"))?;
    let input_file = File::open(cmd.input_path)?;
    let architecture = match cmd.input_format {
        InputFormat::Tarball => {
            // Load it as a tarball.
            let input_archive = tar::Archive::new(input_file);

            image = image.add_layer(input_archive, false)?;

            // The target architecture must have been explicitly passed from command line arguments.
            cmd.arch.context("missing --arch option")?
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

            // Load each layer.
            for (i, layer) in input_archive.layers()?.enumerate() {
                let handle_whiteouts = i != 0; // the base layer cannot contain whiteouts
                image = image.add_layer(layer, handle_whiteouts)?;
            }

            // Read the architecture from the archive's metadata.
            let arch_from_metadata = match input_archive.architecture() {
                DockerArchiveArchitecture::Amd64 => ContainerArchitecture::X64,
                DockerArchiveArchitecture::Arm64 => ContainerArchitecture::Arm64,
            };
            if cmd.arch.is_some_and(|given| given != arch_from_metadata) {
                bail!("The --arch value does not match the contents of the input archive");
            }
            arch_from_metadata
        }
    };

    // Add the default_init executable corresponding to the target architecture.
    let default_init = match architecture {
        ContainerArchitecture::Arm64 => DEFAULT_INIT_ARM64,
        ContainerArchitecture::X64 => DEFAULT_INIT_X64,
    };
    package_builder.add_contents_as_blob("data/init", default_init, temp_dir.path())?;

    // Add the container's manifest and ensure that all of its mountpoints actually exist.
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
    image = image.ensure_directory_exists("/dev")?;
    image = image.ensure_directory_exists("/proc")?;
    image = image.ensure_directory_exists("/sys")?;
    image = image.ensure_directory_exists("/tmp")?;

    // Assign inode numbers and get the root directory.
    let root_dir = {
        let mut next_inode_num = ROOT_INODE_NUM;
        let mut inode_num_generator = || {
            let result = next_inode_num;
            next_inode_num += 1;
            result
        };
        image.finalize(&mut inode_num_generator)
    };
    assert_eq!(root_dir.metadata().inode_num(), ROOT_INODE_NUM);

    // Convert the hierarchy we just loaded into an ext4 Metadata object and its blobs.
    let mut bundle_visitor = PopulateBundleVisitor {
        package_builder,
        ext4_metadata: ext4_metadata::Metadata::new(),
        visited_inode_nums: HashSet::new(),
        name_stack: Vec::new(),
    };
    bundle_visitor.ext4_metadata.insert_directory(
        ROOT_INODE_NUM,
        root_dir.metadata().mode() | S_IFDIR,
        root_dir.metadata().uid(),
        root_dir.metadata().gid(),
        root_dir.metadata().extended_attributes(),
    );
    root_dir.visit(&mut bundle_visitor);

    // Serialize and write the ext4 Metadata object into the archive.
    let metadata_v1 = bundle_visitor.ext4_metadata.serialize();
    let mut package_builder = bundle_visitor.package_builder;
    package_builder.add_contents_as_blob(
        "data/rootfs/metadata.v1",
        metadata_v1,
        temp_dir.path(),
    )?;

    // Write the archive into the output file.
    let gendir = temp_dir.path().join("gen");
    let manifest = package_builder.build(gendir, temp_dir.path().join("meta.far"))?;
    manifest.archive(temp_dir.path(), File::create(cmd.output_file)?).await?;

    Ok(())
}
