// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::magma::MagmaFile,
    fs::{FileOps, FsNode},
    task::CurrentTask,
    types::*,
};

pub fn create_magma_device(
    current_task: &CurrentTask,
    id: DeviceType,
    node: &FsNode,
    flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    MagmaFile::new_file(current_task, id, node, flags)
}
