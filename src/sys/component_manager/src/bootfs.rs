// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::AsHandleRef as _,
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_bootfs::{BootfsParser, BootfsParserError},
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    fuchsia_zircon::{self as zx, HandleBased, Resource},
    std::convert::{From, TryFrom},
    std::sync::Arc,
    thiserror::Error,
    tracing::info,
    vfs::{
        directory::{entry::DirectoryEntry, immutable::connection::io1::ImmutableConnection},
        execution_scope::ExecutionScope,
        file::vmo,
        tree_builder::{self, TreeBuilder},
        ToObjectRequest,
    },
};

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

    // BootfsSvc can be used in hermetic integration tests by providing
    // an arbitrary vmo containing a valid bootfs image.
    pub fn new_for_test(bootfs: zx::Vmo) -> Result<Self, BootfsError> {
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
    ) -> Result<Arc<dyn DirectoryEntry>, BootfsError> {
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

    fn create_dir_entry(vmo: zx::Vmo, executable: bool, inode: u64) -> Arc<dyn DirectoryEntry> {
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

    pub fn ingest_bootfs_vmo(self, system: &Option<Resource>) -> Result<Self, BootfsError> {
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

    // Ingesting the bootfs vmo with this API will produce a /boot VFS that supports all
    // functionality except execution of contents; the permission to convert arbitrary vmos
    // to executable requires the root System resource, which is not available to fuchsia
    // test components.
    pub fn ingest_bootfs_vmo_for_test(self) -> Result<Self, BootfsError> {
        let fake_exec: zx::Vmo = self
            .bootfs
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(BootfsError::DuplicateHandle)?
            .into();
        self.ingest_bootfs_vmo_internal(fake_exec)
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
            let flags = fio::OpenFlags::RIGHT_READABLE
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
