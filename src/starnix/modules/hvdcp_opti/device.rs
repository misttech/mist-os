// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::CurrentTask;
use starnix_sync::{FileOpsCore, LockBefore, Locked};

pub fn hvdcp_opti_init<L>(_locked: &mut Locked<'_, L>, _current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
}
