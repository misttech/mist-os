// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::AsHandleRef as _;
use fuchsia_bootfs::{BootfsParser, BootfsParserError};
use fuchsia_component::client;
use fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType};
use fuchsia_zircon::{self as zx, HandleBased, Resource};
use std::sync::Arc;
use thiserror::Error;
use tracing::info;
use vfs::directory::immutable::connection::ImmutableConnection;
use vfs::execution_scope::ExecutionScope;
use vfs::file::vmo;
use vfs::tree_builder::{self, TreeBuilder};
use vfs::ToObjectRequest;
use {fidl_fuchsia_io as fio, fidl_fuchsia_kernel as fkernel, fuchsia_async as fasync};

// Used to create executable VMOs.
const BOOTFS_VMEX_NAME: &str = "bootfs_vmex";

// If passed as a kernel handle, this gets relocated into a '/boot/log' directory.
const KERNEL_CRASHLOG_NAME: &str = "crashlog";
const LAST_PANIC_FILEPATH: &str = "log/last-panic.txt";

// Kernel startup VMOs are published beneath '/boot/kernel'. The VFS is relative
// to '/boot', so we only need to prepend paths under that.
const KERNEL_VMO_SUBDIRECTORY: &str = "kernel/";

// Bootfs will sequentially number files and directories starting with this value.
// This is a self contained immutable filesystem, so we only need to ensure that
// there are no internal collisions.
const FIRST_INODE_VALUE: u64 = 1;

// Packages in bootfs can contain both executable and read-only files. For example,
// 'pkg/my_package/bin' should be executable but 'pkg/my_package/foo' should not.
const BOOTFS_PACKAGE_PREFIX: &str = "pkg";
const BOOTFS_EXECUTABLE_PACKAGE_DIRECTORIES: &[&str] = &["bin", "lib"];

// Top level directories in bootfs that are allowed to contain executable files.
// Every file in these directories will have ZX_RIGHT_EXECUTE.
const BOOTFS_EXECUTABLE_DIRECTORIES: &[&str] = &["bin", "driver", "lib", "test", "blob"];

#[derive(Debug, Error)]
pub enum BootfsError {
    #[error("Invalid handle: {handle_type:?}")]
    InvalidHandle { handle_type: HandleType },
    #[error("Failed to duplicate handle: {0}")]
    DuplicateHandle(zx::Status),
    #[error("Failed to access vmex Resource: {0}")]
    Vmex(zx::Status),
    #[error("BootfsParser error: {0}")]
    Parser(#[from] BootfsParserError),
    #[error("Failed to add entry to Bootfs VFS: {0}")]
    AddEntry(#[from] tree_builder::Error),
    #[error("Failed to locate entry for {name}")]
    MissingEntry { name: String },
    #[error("Failed to create an executable VMO: {0}")]
    ExecVmo(zx::Status),
    #[error("VMO operation failed: {0}")]
    Vmo(zx::Status),
    #[error("Failed to get VMO name: {0}")]
    VmoName(zx::Status),
    #[error("Failed to create VMO child at offset {offset}: {err}")]
    VmoCreateChild { offset: u64, err: zx::Status },
    #[error("Failed to convert numerical value: {0}")]
    ConvertNumber(#[from] std::num::TryFromIntError),
    #[error("Failed to convert string value: {0}")]
    ConvertString(#[from] std::ffi::IntoStringError),
    #[error("Failed to bind Bootfs to Component Manager's namespace: {0}")]
    Namespace(zx::Status),
}

pub struct BootfsSvc {
    next_inode: u64,
    parser: BootfsParser,
    bootfs: zx::Vmo,
    tree_builder: TreeBuilder,
}

impl BootfsSvc {
    pub fn new() -> Result<Self, BootfsError> {
        let bootfs_handle = take_startup_handle(HandleType::BootfsVmo.into())
            .ok_or(BootfsError::InvalidHandle { handle_type: HandleType::BootfsVmo })?;
        let bootfs = zx::Vmo::from(bootfs_handle);
        Self::new_internal(bootfs)
    }

    fn new_internal(bootfs: zx::Vmo) -> Result<Self, BootfsError> {
        let bootfs_dup = bootfs
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(BootfsError::DuplicateHandle)?
            .into();
        let parser = BootfsParser::create_from_vmo(bootfs_dup)?;
        Ok(Self {
            next_inode: FIRST_INODE_VALUE,
            parser,
            bootfs,
            tree_builder: TreeBuilder::empty_dir(),
        })
    }

    fn get_next_inode(inode: &mut u64) -> u64 {
        let next_inode = *inode;
        *inode += 1;
        next_inode
    }

    fn file_in_executable_directory(path: &Vec<&str>) -> bool {
        // If the first token is 'pkg', the second token can be anything, with the third
        // token needing to be within the list of allowed executable package directories.
        if path.len() > 2 && path[0] == BOOTFS_PACKAGE_PREFIX {
            if BOOTFS_EXECUTABLE_PACKAGE_DIRECTORIES.iter().any(|dir| path[2] == *dir) {
                return true;
            }
        }
        // If the first token is an allowed executable directory, everything beneath it
        // can be marked executable.
        if path.len() > 1 {
            if BOOTFS_EXECUTABLE_DIRECTORIES.iter().any(|dir| path[0] == *dir) {
                return true;
            }
        }
        false
    }

    fn create_dir_entry_with_child(
        parent: &zx::Vmo,
        offset: u64,
        size: u64,
        executable: bool,
        inode: u64,
    ) -> Result<Arc<vmo::VmoFile>, BootfsError> {
        // If this is a VMO with execution rights, passing zx::VmoChildOptions::NO_WRITE will
        // allow the child to also inherit execution rights. Without that flag execution
        // rights are stripped, even if the VMO already lacked write permission.
        let child = parent
            .create_child(
                zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE | zx::VmoChildOptions::NO_WRITE,
                offset,
                size,
            )
            .map_err(|err| BootfsError::VmoCreateChild { offset, err })?;
        Ok(BootfsSvc::create_dir_entry(child, executable, inode))
    }

    fn create_dir_entry(vmo: zx::Vmo, executable: bool, inode: u64) -> Arc<vmo::VmoFile> {
        vmo::VmoFile::new_with_inode(
            vmo, /*readable*/ true, /*writable*/ false, executable, inode,
        )
    }

    /// Read configs from the parsed bootfs image before the filesystem has been fully initialized.
    /// This is required for configs needed to run the VFS, such as the component manager config
    /// which specifies the number of threads for the executor which the VFS needs to run within.
    /// Path should be relative to '/boot' without a leading forward slash.
    pub fn read_config_from_uninitialized_vfs(
        &self,
        config_path: &str,
    ) -> Result<Vec<u8>, BootfsError> {
        for entry in self.parser.zero_copy_iter() {
            let entry = entry?;
            assert!(entry.payload.is_none()); // Using the zero copy iterator.
            if entry.name == config_path {
                let mut buffer = vec![0; usize::try_from(entry.size)?];
                self.bootfs.read(&mut buffer, entry.offset).map_err(BootfsError::Vmo)?;
                return Ok(buffer);
            }
        }
        Err(BootfsError::MissingEntry { name: config_path.to_string() })
    }

    pub fn ingest_bootfs_vmo_with_system_resource(
        self,
        system: &Option<Resource>,
    ) -> Result<Self, BootfsError> {
        let system = system
            .as_ref()
            .ok_or(BootfsError::InvalidHandle { handle_type: HandleType::Resource })?;

        let vmex = system
            .create_child(
                zx::ResourceKind::SYSTEM,
                None,
                zx::sys::ZX_RSRC_SYSTEM_VMEX_BASE,
                1,
                BOOTFS_VMEX_NAME.as_bytes(),
            )
            .map_err(BootfsError::Vmex)?;
        self.ingest_bootfs_vmo(vmex)
    }

    pub async fn ingest_bootfs_vmo_with_namespace_vmex(self) -> Result<Self, BootfsError> {
        let vmex_service = client::connect_to_protocol::<fkernel::VmexResourceMarker>()
            .map_err(|_| BootfsError::Vmex(zx::Status::UNAVAILABLE))?;
        let vmex =
            vmex_service.get().await.map_err(|_| BootfsError::Vmex(zx::Status::UNAVAILABLE))?;
        self.ingest_bootfs_vmo(vmex)
    }

    fn ingest_bootfs_vmo(self, vmex: Resource) -> Result<Self, BootfsError> {
        // The bootfs VFS is comprised of multiple child VMOs which are just offsets into a
        // single backing parent VMO.
        //
        // The parent VMO is duplicated here and marked as executable to reduce the total
        // number of syscalls required. Files in directories that are read-only will just
        // be children of the original read-only VMO, and files in directories that are
        // read-execution will be children of the duplicated read-execution VMO.
        let bootfs_exec: zx::Vmo = self
            .bootfs
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(BootfsError::DuplicateHandle)?
            .into();
        let bootfs_exec = bootfs_exec.replace_as_executable(&vmex).map_err(BootfsError::ExecVmo)?;

        self.ingest_bootfs_vmo_internal(bootfs_exec)
    }

    pub fn ingest_bootfs_vmo_internal(mut self, bootfs_exec: zx::Vmo) -> Result<Self, BootfsError> {
        for entry in self.parser.zero_copy_iter() {
            let entry = entry?;
            assert!(entry.payload.is_none()); // Using the zero copy iterator.

            let name = entry.name;
            let path_parts: Vec<&str> = name.split("/").filter(|&x| !x.is_empty()).collect();

            let is_exec = BootfsSvc::file_in_executable_directory(&path_parts);
            let vmo = if is_exec { &bootfs_exec } else { &self.bootfs };
            let dir_entry = BootfsSvc::create_dir_entry_with_child(
                vmo,
                entry.offset,
                entry.size,
                is_exec,
                BootfsSvc::get_next_inode(&mut self.next_inode),
            )?;
            self.tree_builder.add_entry(&path_parts, dir_entry)?;
        }

        Ok(self)
    }

    // Publish a VMO beneath '/boot/kernel'. Used to publish VDSOs and kernel files.
    pub fn publish_kernel_vmo(mut self, vmo: zx::Vmo) -> Result<Self, BootfsError> {
        let name = vmo.get_name().map_err(BootfsError::VmoName)?.into_string()?;
        if name.is_empty() {
            // Skip VMOs without names.
            return Ok(self);
        }
        let path = format!("{}{}", KERNEL_VMO_SUBDIRECTORY, name);
        let mut path_parts: Vec<&str> = path.split("/").filter(|&x| !x.is_empty()).collect();

        // There is special handling for the crashlog.
        if path_parts.len() > 1 && path_parts[path_parts.len() - 1] == KERNEL_CRASHLOG_NAME {
            path_parts = LAST_PANIC_FILEPATH.split("/").filter(|&x| !x.is_empty()).collect();
        }

        let vmo_size = vmo.get_size().map_err(BootfsError::Vmo)?;
        if vmo_size == 0 {
            // Skip empty VMOs.
            return Ok(self);
        }

        // If content size is zero, set it to the size of the VMO.
        if vmo.get_content_size().map_err(BootfsError::Vmo)? == 0 {
            vmo.set_content_size(&vmo_size).map_err(BootfsError::Vmo)?;
        }

        let info = vmo.basic_info().map_err(BootfsError::Vmo)?;
        let is_exec = info.rights.contains(zx::Rights::EXECUTE);

        let dir_entry = BootfsSvc::create_dir_entry(
            vmo,
            is_exec,
            BootfsSvc::get_next_inode(&mut self.next_inode),
        );
        self.tree_builder.add_entry(&path_parts, dir_entry)?;

        Ok(self)
    }

    /// Publish all VMOs of a given type provided to this process through its processargs
    /// bootstrap message. An initial index can be provided to skip handles that were already
    /// taken.
    pub fn publish_kernel_vmos(
        mut self,
        handle_type: HandleType,
        first_index: u16,
    ) -> Result<Self, BootfsError> {
        info!(
            "[BootfsSvc] Adding kernel VMOs of type {:?} starting at index {}.",
            handle_type, first_index
        );
        // The first handle may not be at index 0 if we have already taken it previously.
        let mut index = first_index;
        loop {
            let vmo = take_startup_handle(HandleInfo::new(handle_type, index)).map(zx::Vmo::from);
            match vmo {
                Some(vmo) => {
                    index += 1;
                    self = self.publish_kernel_vmo(vmo)?;
                }
                None => break,
            }
        }

        Ok(self)
    }

    pub fn create_and_bind_vfs(mut self) -> Result<(), BootfsError> {
        info!("[BootfsSvc] Finalizing rust bootfs service.");

        let (directory, directory_server_end) = fidl::endpoints::create_endpoints();

        let mut get_inode = |_| -> u64 { BootfsSvc::get_next_inode(&mut self.next_inode) };

        let vfs = self.tree_builder.build_with_inode_generator(&mut get_inode);

        // Run the service with its own executor to avoid reentrancy issues.
        std::thread::spawn(move || {
            let flags: fio::OpenFlags = fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_EXECUTABLE
                | fio::OpenFlags::DIRECTORY;
            fasync::LocalExecutor::new().run_singlethreaded(
                flags
                    .to_object_request(directory_server_end)
                    .handle(|object_request| {
                        ImmutableConnection::create(
                            ExecutionScope::new(),
                            vfs,
                            flags,
                            object_request,
                        )
                    })
                    .unwrap(),
            );
        });

        let ns = fdio::Namespace::installed().map_err(BootfsError::Namespace)?;
        assert!(
            ns.unbind("/boot").is_err(),
            "No filesystem should already be bound to /boot when BootfsSvc is starting."
        );

        ns.bind("/boot", directory).map_err(BootfsError::Namespace)?;

        info!("[BootfsSvc] Bootfs is ready and is now serving /boot.");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Error};
    use fidl_fuchsia_io as fio;
    use fuchsia_fs::{directory, file, node, OpenFlags};
    use futures::StreamExt;
    use libc::{S_IRUSR, S_IXUSR};

    // Since this is a system test, we're actually going to verify real system critical files. That
    // means that these tests take a dependency on these files existing in the system, which may
    // not forever be true. If any of the files listed here are removed, it's fine to update the set
    // of checked files.
    const SAMPLE_UTF8_READONLY_FILE: &str = "/boot/config/build_info/minimum_utc_stamp";
    const SAMPLE_REQUIRED_DIRECTORY: &str = "/boot/lib";
    const KERNEL_VDSO_DIRECTORY: &str = "/boot/kernel/vdso";
    const BOOTFS_READONLY_FILES: &[&str] = &["/boot/config/mistos_elf_runner"];
    const BOOTFS_DATA_DIRECTORY: &str = "/boot/data";
    const BOOTFS_EXECUTABLE_LIB_FILES: &[&str] = &["ld.so.1"];
    const BOOTFS_EXECUTABLE_NON_LIB_FILES: &[&str] = &["/boot/bin/mistos_elf_runner"];

    #[fuchsia::test]
    async fn basic_filenode_test() -> Result<(), Error> {
        // Open the known good file as a node, and check its attributes.
        let node = node::open_in_namespace(SAMPLE_UTF8_READONLY_FILE, OpenFlags::RIGHT_READABLE)
            .context("failed to open as a readable node")?;

        // This node should be a readonly file, the inode should not be unknown,
        // and creation and modification times should be 0 since system UTC
        // isn't available or reliable in early boot.
        assert_eq!(node.get_attr().await?.1.mode, fio::MODE_TYPE_FILE | S_IRUSR);
        assert_ne!(node.get_attr().await?.1.id, fio::INO_UNKNOWN);
        assert_eq!(node.get_attr().await?.1.creation_time, 0);
        assert_eq!(node.get_attr().await?.1.modification_time, 0);

        node::close(node).await?;

        // Reopen the known good file as a file to make use of the helper functions.
        let file = file::open_in_namespace(SAMPLE_UTF8_READONLY_FILE, OpenFlags::RIGHT_READABLE)
            .context("failed to open as a readable file")?;

        // Check for data corruption. This file should contain a single utf-8 string which can
        // be converted into a non-zero unsigned integer.
        let file_contents =
            file::read_to_string(&file).await.context("failed to read utf-8 file to string")?;
        let parsed_time = file_contents
            .trim()
            .parse::<u64>()
            .context("failed to utf-8 string as a number (and it should be a number!)")?;
        assert_ne!(parsed_time, 0);

        file::close(file).await?;

        Ok(())
    }

    #[fuchsia::test]
    async fn basic_directory_test() -> Result<(), Error> {
        // Open the known good file as a node, and check its attributes.
        let node = node::open_in_namespace(
            SAMPLE_REQUIRED_DIRECTORY,
            OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
        )
        .context("failed to open as a readable and executable node")?;

        // This node should be an immutable directory, the inode should not be unknown,
        // and creation and modification times should be 0 since system UTC isn't
        // available or reliable in early boot.
        assert_ne!(node.get_attr().await?.1.id, fio::INO_UNKNOWN);
        assert_eq!(node.get_attr().await?.1.creation_time, 0);
        assert_eq!(node.get_attr().await?.1.modification_time, 0);

        // TODO(https://fxbug.dev/42173193): The C++ bootfs VFS uses the wrong POSIX bits (needs S_IXUSR).
        let cpp_bootfs = fio::MODE_TYPE_DIRECTORY | S_IRUSR;
        let rust_bootfs = fio::MODE_TYPE_DIRECTORY | S_IRUSR | S_IXUSR;
        let actual_value = node.get_attr().await?.1.mode;
        assert!(actual_value == cpp_bootfs || actual_value == rust_bootfs);

        node::close(node).await?;

        Ok(())
    }

    #[fuchsia::test]
    async fn check_kernel_vmos() -> Result<(), Error> {
        let directory = directory::open_in_namespace(
            KERNEL_VDSO_DIRECTORY,
            OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
        )
        .context("failed to open kernel vdso directory")?;
        let vdsos = fuchsia_fs::directory::readdir(&directory)
            .await
            .context("failed to read kernel vdso directory")?;

        // We should have added at least the default VDSO.
        assert_ne!(vdsos.len(), 0);
        directory::close(directory).await?;

        // All VDSOs should have execution rights.
        for vdso in vdsos {
            let name = format!("{}/{}", KERNEL_VDSO_DIRECTORY, vdso.name);
            let file = file::open_in_namespace(
                &name,
                OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
            )
            .context("failed to open file")?;
            let data = file::read_num_bytes(&file, 1).await.context(format!(
                "failed to read a single byte from a vdso opened as read-execute: {}",
                name
            ))?;
            assert_ne!(data.len(), 0);
            file::close(file).await?;
        }

        Ok(())
    }

    #[fuchsia::test]
    async fn check_executable_files() -> Result<(), Error> {
        // Sanitizers nest lib files within '/boot/lib/asan' or '/boot/lib/asan-ubsan' etc., so
        // we need to just search recursively for these files instead.
        let directory = directory::open_in_namespace(
            "/boot/lib",
            OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
        )
        .context("failed to open /boot/lib directory")?;
        let lib_paths = fuchsia_fs::directory::readdir_recursive(&directory, None)
            .filter_map(|result| async {
                assert!(result.is_ok());
                let entry = result.unwrap();
                for file in BOOTFS_EXECUTABLE_LIB_FILES {
                    if entry.name.ends_with(file) {
                        return Some(format!("/boot/lib/{}", entry.name));
                    }
                }

                None
            })
            .collect::<Vec<String>>()
            .await;
        directory::close(directory).await?;

        // Should have found all of the library files.
        assert_eq!(lib_paths.len(), BOOTFS_EXECUTABLE_LIB_FILES.len());
        let paths = [
            lib_paths,
            BOOTFS_EXECUTABLE_NON_LIB_FILES.iter().map(|val| val.to_string()).collect::<Vec<_>>(),
        ]
        .concat();

        for path in paths {
            let file = file::open_in_namespace(
                &path,
                OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
            )
            .context("failed to open file")?;
            let data = file::read_num_bytes(&file, 1).await.context(format!(
                "failed to read a single byte from a file opened as read-execute: {}",
                path
            ))?;
            assert_ne!(data.len(), 0);
            file::close(file).await?;
        }

        Ok(())
    }

    #[fuchsia::test]
    async fn check_readonly_files() -> Result<(), Error> {
        // There is a large variation in what different products have in the data directory, so
        // just search it during the test time and find some files. Every file in the data directory
        // should be readonly.
        let directory = directory::open_in_namespace(
            BOOTFS_DATA_DIRECTORY,
            OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
        )
        .context("failed to open data directory")?;
        let data_paths = fuchsia_fs::directory::readdir_recursive(&directory, None)
            .filter_map(|result| async {
                assert!(result.is_ok());
                Some(format!("{}/{}", BOOTFS_DATA_DIRECTORY, result.unwrap().name))
            })
            .collect::<Vec<String>>()
            .await;
        directory::close(directory).await?;

        let paths = [
            data_paths,
            BOOTFS_READONLY_FILES.iter().map(|val| val.to_string()).collect::<Vec<_>>(),
        ]
        .concat();

        for path in paths {
            // A readonly file should not be usable when opened as executable.
            let mut file = file::open_in_namespace(
                &path,
                OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_EXECUTABLE,
            )
            .context("failed to open file")?;
            let result = file::read_num_bytes(&file, 1).await;
            assert!(result.is_err());
            // Don't close the file proxy -- the access error above has already closed the channel.

            // Reopen as readonly, and confirm that it can be read from.
            file = file::open_in_namespace(&path, OpenFlags::RIGHT_READABLE)
                .context("failed to open file")?;
            let data = file::read_num_bytes(&file, 1).await.context(format!(
                "failed to read a single byte from a file opened as readonly: {}",
                path
            ))?;
            assert_ne!(data.len(), 0);
            file::close(file).await?;
        }

        Ok(())
    }
}
