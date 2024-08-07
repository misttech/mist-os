// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_process as fprocess;
use fuchsia_runtime::{HandleInfo, HandleType};
use starnix_core::fs::fuchsia::{create_file_from_handle, SyslogFile};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FdNumber, FdTable};

/// Adds the given startup handles to a CurrentTask.
///
/// The `numbered_handles` of type `HandleType::FileDescriptor` are used to
/// create files, and the handles are required to be of type `zx::Socket`.
///
/// If there is a `numbered_handles` of type `HandleType::User0`, that is
/// interpreted as the server end of the ShellController protocol.
pub fn parse_numbered_handles(
    current_task: &CurrentTask,
    numbered_handles: Option<Vec<fprocess::HandleInfo>>,
    files: &FdTable,
) -> Result<(), Error> {
    if let Some(numbered_handles) = numbered_handles {
        for numbered_handle in numbered_handles {
            let info = HandleInfo::try_from(numbered_handle.id)?;
            if info.handle_type() == HandleType::FileDescriptor {
                files.insert(
                    current_task,
                    FdNumber::from_raw(info.arg().into()),
                    create_file_from_handle(current_task, numbered_handle.handle)?,
                )?;
            }
        }
    }

    let stdio = SyslogFile::new_file(current_task);
    // If no numbered handle is provided for each stdio handle, default to syslog.
    for i in [0, 1, 2] {
        if files.get(FdNumber::from_raw(i)).is_err() {
            files.insert(current_task, FdNumber::from_raw(i), stdio.clone())?;
        }
    }

    Ok(())
}

/*
/// A record of the mounts created when starting a component.
///
/// When the record is dropped, the mounts are unmounted.
#[derive(Default)]
struct MountRecord {
    /// The namespace nodes at which we have crated mounts for this component.
    mounts: Vec<NamespaceNode>,
}

impl MountRecord {
    fn mount(
        &mut self,
        mount_point: NamespaceNode,
        what: WhatToMount,
        flags: MountFlags,
    ) -> Result<(), Errno> {
        mount_point.mount(what, flags)?;
        self.mounts.push(mount_point);
        Ok(())
    }

    fn mount_remote<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        system_task: &CurrentTask,
        directory: &fio::DirectorySynchronousProxy,
        path: &str,
    ) -> Result<(), Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        // The incoming dir_path might not be top level, e.g. it could be /foo/bar.
        // Iterate through each component directory starting from the parent and
        // create it if it doesn't exist.
        let mut current_node = system_task.lookup_path_from_root(".".into())?;
        let mut context = LookupContext::default();

        // Extract each component using Path::new(path).components(). For example,
        // Path::new("/foo/bar").components() will return [RootDir, Normal("foo"), Normal("bar")].
        // We're not interested in the RootDir, so we drop the prefix "/" if it exists.
        let path = if let Some(path) = path.strip_prefix('/') { path } else { path };

        for sub_dir in Path::new(path).components() {
            let sub_dir = sub_dir.as_os_str().as_bytes();

            current_node = match current_node.create_node(
                locked,
                system_task,
                sub_dir.into(),
                mode!(IFDIR, 0o755),
                DeviceType::NONE,
            ) {
                Ok(node) => node,
                Err(errno) if errno == EEXIST || errno == ENOTDIR => {
                    current_node.lookup_child(system_task, &mut context, sub_dir.into())?
                }
                Err(e) => bail!(e),
            };
        }

        let (status, rights) = directory.get_flags(zx::Time::INFINITE)?;
        zx::Status::ok(status)?;

        let (client_end, server_end) = zx::Channel::create();
        directory.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, ServerEnd::new(server_end))?;

        let fs = RemoteFs::new_fs(
            system_task.kernel(),
            client_end,
            FileSystemOptions { source: path.into(), ..Default::default() },
            rights,
        )?;
        // Fuchsia doesn't specify mount flags in the incoming namespace, so we need to make
        // up some flags.
        let flags = MountFlags::NOSUID | MountFlags::NODEV | MountFlags::RELATIME;
        current_node.mount(WhatToMount::Fs(fs), flags)?;
        self.mounts.push(current_node);

        Ok(())
    }

    fn unmount(&mut self) -> Result<(), Errno> {
        while let Some(node) = self.mounts.pop() {
            node.unmount(UnmountFlags::DETACH)?;
        }
        Ok(())
    }
}

impl Drop for MountRecord {
    fn drop(&mut self) {
        match self.unmount() {
            Ok(()) => {}
            Err(e) => log_error!("failed to unmount during component exit: {:?}", e),
        }
    }
}
    */
