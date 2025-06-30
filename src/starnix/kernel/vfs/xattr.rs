// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{FsStr, FsString, XattrOp, XattrStorage};
use starnix_sync::{FileOpsCore, Locked, MemoryXattrStorageLevel, OrderedMutex};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};
use std::collections::hash_map::Entry;
use std::collections::HashMap;

#[derive(Default)]
pub struct MemoryXattrStorage {
    xattrs: OrderedMutex<HashMap<FsString, FsString>, MemoryXattrStorageLevel>,
}

impl XattrStorage for MemoryXattrStorage {
    fn get_xattr(&self, locked: &mut Locked<FileOpsCore>, name: &FsStr) -> Result<FsString, Errno> {
        let xattrs = self.xattrs.lock(locked);
        Ok(xattrs.get(name).ok_or_else(|| errno!(ENODATA))?.clone())
    }

    fn set_xattr(
        &self,
        locked: &mut Locked<FileOpsCore>,
        name: &FsStr,
        value: &FsStr,
        op: XattrOp,
    ) -> Result<(), Errno> {
        let mut xattrs = self.xattrs.lock(locked);
        match xattrs.entry(name.to_owned()) {
            Entry::Vacant(_) if op == XattrOp::Replace => return error!(ENODATA),
            Entry::Occupied(_) if op == XattrOp::Create => return error!(EEXIST),
            Entry::Vacant(v) => {
                v.insert(value.to_owned());
            }
            Entry::Occupied(mut o) => {
                let s = o.get_mut();
                s.clear();
                s.extend_from_slice(value);
            }
        };
        Ok(())
    }

    fn remove_xattr(&self, locked: &mut Locked<FileOpsCore>, name: &FsStr) -> Result<(), Errno> {
        let mut xattrs = self.xattrs.lock(locked);
        if xattrs.remove(name).is_none() {
            return error!(ENODATA);
        }
        Ok(())
    }

    fn list_xattrs(&self, locked: &mut Locked<FileOpsCore>) -> Result<Vec<FsString>, Errno> {
        let xattrs = self.xattrs.lock(locked);
        Ok(xattrs.keys().cloned().collect())
    }
}
