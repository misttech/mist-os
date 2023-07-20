// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ext4_metadata::{ExtendedAttributes, ROOT_INODE_NUM};
use static_assertions::const_assert;
use std::io::Result;
use {ext4_extract::remote_bundle, ext4_extract::remote_bundle::Owner};

/// Read-only by everyone because `remote_bundle`s are never writeable and we don't put anything
/// executable in HAL metadata.
const FILE_MODE: u16 = 0o0444 + linux_uapi::S_IFREG as u16;
const_assert!(linux_uapi::S_IFREG < 2_u32.pow(16)); // catch overflow

/// Read-only and enterable by everyone because `remote_bundle`s are never writeable.
const DIRECTORY_MODE: u16 = 0o0555 + linux_uapi::S_IFDIR as u16;
const_assert!(linux_uapi::S_IFDIR < 2_u32.pow(16)); // catch overflow

pub struct Writer {
    pub inner: remote_bundle::Writer,
    next_inode: u64,
}

/// Wrapper over [remote_bundle::Writer] that supplies inode numbers and attributes
/// commonly used in an android image.
impl Writer {
    /// Creates a remote bundle writer which will store its files at `out_dir`.
    pub fn new(out_dir: &impl AsRef<str>) -> Result<Writer> {
        Ok(Writer {
            inner: remote_bundle::Writer::new(
                out_dir,
                ROOT_INODE_NUM,
                DIRECTORY_MODE,
                Owner::root(),
                ExtendedAttributes::new(),
            )?,
            next_inode: ROOT_INODE_NUM + 1,
        })
    }

    /// Add the contents of `data` as a file at `path` in the remote bundle.
    pub fn add_file(&mut self, path: &[&str], data: &mut impl std::io::Read) -> Result<()> {
        let inode = self.alloc_inode();
        self.inner.add_file(path, data, inode, FILE_MODE, Owner::root(), ExtendedAttributes::new())
    }

    /// Add an empty directory at `path` in the remote bundle.
    pub fn add_directory(&mut self, path: &[&str]) {
        let inode = self.alloc_inode();
        self.inner.add_directory(
            path,
            inode,
            DIRECTORY_MODE,
            Owner::root(),
            ExtendedAttributes::new(),
        )
    }

    fn alloc_inode(&mut self) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        inode
    }
}
