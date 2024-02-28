// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common trait for all the directory entry objects.

#![warn(missing_docs)]

use crate::{common::IntoAny, node::IsDirectory};

use {
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    std::{fmt, sync::Arc},
};

pub use super::simple::OpenRequest;

/// Information about a directory entry, used to populate ReadDirents() output.
/// The first element is the inode number, or INO_UNKNOWN (from fuchsia.io) if not set, and the second
/// element is one of the DIRENT_TYPE_* constants defined in the fuchsia.io.
#[derive(PartialEq, Eq, Clone)]
pub struct EntryInfo(u64, fio::DirentType);

impl EntryInfo {
    /// Constructs a new directory entry information object.
    pub fn new(inode: u64, type_: fio::DirentType) -> Self {
        Self(inode, type_)
    }

    /// Retrives the `inode` argument of the [`EntryInfo::new()`] constructor.
    pub fn inode(&self) -> u64 {
        let Self(inode, _type) = self;
        *inode
    }

    /// Retrieves the `type_` argument of the [`EntryInfo::new()`] constructor.
    pub fn type_(&self) -> fio::DirentType {
        let Self(_inode, type_) = self;
        *type_
    }
}

impl fmt::Debug for EntryInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(inode, type_) = self;
        if *inode == fio::INO_UNKNOWN {
            write!(f, "{:?}(fio::INO_UNKNOWN)", type_)
        } else {
            write!(f, "{:?}({})", type_, inode)
        }
    }
}

/// Pseudo directories contain items that implement this trait.  Pseudo directories refer to the
/// items they contain as `Arc<dyn DirectoryEntry>`.
///
/// *NOTE*: This trait only needs to be implemented if you want to add your nodes to a pseudo
/// directory.  If you don't need to add your nodes to a pseudo directory, consider implementing
/// node::IsDirectory instead.
pub trait DirectoryEntry: IntoAny + Sync + Send + 'static {
    /// This method is used to populate ReadDirents() output.
    fn entry_info(&self) -> EntryInfo;

    /// Opens this entry.
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status>;
}

impl<T: DirectoryEntry> IsDirectory for T {
    fn is_directory(&self) -> bool {
        self.entry_info().type_() == fio::DirentType::Directory
    }
}
