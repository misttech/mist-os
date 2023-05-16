// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::fs::FileHandle;
use crate::task::CurrentTask;
use crate::types::*;

mod remote;
mod remote_bundle;
mod syslog;
mod timer;

pub use remote::*;
pub use remote_bundle::RemoteBundle;
pub use syslog::*;
pub use timer::*;

/// Create a FileHandle from a zx::Handle.
pub fn create_file_from_handle(
    current_task: &CurrentTask,
    handle: zx::Handle,
) -> Result<FileHandle, Errno> {
    new_remote_file(current_task.kernel(), handle, OpenFlags::RDWR)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;
    use zx::HandleBased;

    #[::fuchsia::test]
    async fn test_create_from_invalid_handle() {
        let (_kernel, current_task) = create_kernel_and_task();
        assert!(create_file_from_handle(&current_task, zx::Handle::invalid()).is_err());
    }

    #[::fuchsia::test]
    async fn test_create_pipe_from_handle() {
        let (_kernel, current_task) = create_kernel_and_task();
        let (left_handle, right_handle) = zx::Socket::create_stream();
        create_file_from_handle(&current_task, left_handle.into_handle())
            .expect("failed to create left FileHandle");
        create_file_from_handle(&current_task, right_handle.into_handle())
            .expect("failed to create right FileHandle");
    }
}
