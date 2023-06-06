// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{magma::MagmaFile, DeviceOps},
    fs::{FileOps, FsNode},
    task::CurrentTask,
    types::*,
};

// Starnix device is a grouping for devices specific to Starnix.
pub struct StarnixDevice;

impl DeviceOps for StarnixDevice {
    fn open(
        &self,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        match id.minor() {
            STARNIX_MINOR_MAGMA => MagmaFile::new_file(current_task, id, node, flags),
            _ => error!(ENODEV),
        }
    }
}
