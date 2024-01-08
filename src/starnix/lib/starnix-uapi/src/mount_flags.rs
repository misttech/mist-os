// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::uapi;
use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct MountFlags: u32 {
        // per-mountpoint flags
        const RDONLY = uapi::MS_RDONLY;
        const NOEXEC = uapi::MS_NOEXEC;
        const NOSUID = uapi::MS_NOSUID;
        const NODEV = uapi::MS_NODEV;
        const NOATIME = uapi::MS_NOATIME;
        const NODIRATIME = uapi::MS_NODIRATIME;
        const RELATIME = uapi::MS_RELATIME;
        const STRICTATIME = uapi::MS_STRICTATIME;

        // per-superblock flags
        const SILENT = uapi::MS_SILENT;
        const LAZYTIME = uapi::MS_LAZYTIME;
        const SYNCHRONOUS = uapi::MS_SYNCHRONOUS;
        const DIRSYNC = uapi::MS_DIRSYNC;
        const MANDLOCK = uapi::MS_MANDLOCK;

        // mount() control flags
        const REMOUNT = uapi::MS_REMOUNT;
        const BIND = uapi::MS_BIND;
        const REC = uapi::MS_REC;
        const DOWNSTREAM = uapi::MS_SLAVE;
        const SHARED = uapi::MS_SHARED;
        const PRIVATE = uapi::MS_PRIVATE;

        /// Flags stored in Mount state.
        const STORED_ON_MOUNT = Self::RDONLY.bits() | Self::NOEXEC.bits() | Self::NOSUID.bits() |
            Self::NODEV.bits() | Self::NOATIME.bits() | Self::NODIRATIME.bits() | Self::RELATIME.bits();

        /// Flags stored in FileSystem options.
        const STORED_ON_FILESYSTEM = Self::RDONLY.bits() | Self::DIRSYNC.bits() | Self::LAZYTIME.bits() |
            Self::MANDLOCK.bits() | Self::SILENT.bits() | Self::SYNCHRONOUS.bits();

        /// Flags that change be changed with REMOUNT.
        ///
        /// MS_DIRSYNC and MS_SILENT cannot be changed with REMOUNT.
        const CHANGEABLE_WITH_REMOUNT = Self::STORED_ON_MOUNT.bits() | Self::STRICTATIME.bits() |
            Self::MANDLOCK.bits() | Self::LAZYTIME.bits() | Self::SYNCHRONOUS.bits();
    }
}

impl std::fmt::Display for MountFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", if self.contains(Self::RDONLY) { "ro" } else { "rw" })?;
        if self.contains(Self::NOEXEC) {
            write!(f, ",noexec")?;
        }
        if self.contains(Self::NOSUID) {
            write!(f, ",nosuid")?;
        }
        if self.contains(Self::NODEV) {
            write!(f, ",nodev")?
        }
        if self.contains(Self::NOATIME) {
            write!(f, ",noatime")?;
        }
        if self.contains(Self::NOEXEC) {
            write!(f, ",noexec")?;
        }
        if self.contains(Self::SILENT) {
            write!(f, ",silent")?;
        }
        if self.contains(Self::BIND) {
            write!(f, ",bind")?;
        }
        if self.contains(Self::LAZYTIME) {
            write!(f, ",lazytime")?;
        }
        Ok(())
    }
}
