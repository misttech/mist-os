// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_io as fio;
use starnix_core::fs::fuchsia::{create_remotefs_filesystem, RemoteBundle};
use starnix_core::vfs::fs_args::MountParams;
use starnix_core::vfs::{FileSystemCreator, FileSystemHandle, FileSystemOptions, FsString};
use starnix_sync::{BeforeFsNodeAppend, DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::mount_flags::MountFlags;

pub struct MountAction {
    pub path: FsString,
    pub fs: FileSystemHandle,
    pub flags: MountFlags,
}

pub fn create_filesystem_from_spec<L>(
    locked: &mut Locked<'_, L>,
    creator: &impl FileSystemCreator,
    pkg: &fio::DirectorySynchronousProxy,
    spec: &str,
) -> Result<MountAction, Error>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<DeviceOpen>,
    L: LockBefore<BeforeFsNodeAppend>,
{
    let kernel = creator.kernel();

    let mut iter = spec.splitn(4, ':');
    let mount_point =
        iter.next().ok_or_else(|| anyhow!("mount point is missing from {:?}", spec))?;
    let fs_type = iter.next().ok_or_else(|| anyhow!("fs type is missing from {:?}", spec))?;
    let fs_src = match iter.next() {
        Some(src) if !src.is_empty() => src,
        _ => ".",
    };

    let mut params = MountParams::parse(iter.next().unwrap_or("").into())?;
    let flags = params.remove_mount_flags();

    let options = FileSystemOptions {
        source: fs_src.into(),
        flags: flags & MountFlags::STORED_ON_FILESYSTEM,
        params,
    };

    // Default rights for remotefs.
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;

    // The filesystem types handled in this match are the ones that can only be specified in a
    // manifest file, for whatever reason. Anything else is passed to create_filesystem, which is
    // common code that also handles the mount() system call.
    let fs = match fs_type {
        "remote_bundle" => RemoteBundle::new_fs(kernel, pkg, options, rights)?,
        "remotefs" => create_remotefs_filesystem(kernel, pkg, options, rights)?,
        _ => creator.create_filesystem(locked, fs_type.into(), options)?,
    };
    Ok(MountAction { path: mount_point.into(), fs, flags })
}
