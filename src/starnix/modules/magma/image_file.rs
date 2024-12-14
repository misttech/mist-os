// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_ui_composition as fuicomp;
use magma::magma_image_info_t;
use starnix_core::mm::memory::MemoryObject;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{Anon, FileHandle, FileOps, FsNodeInfo};
use starnix_core::{fileops_impl_memory, fileops_impl_noop_sync};
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Arc;
use zx::{AsHandleRef, HandleBased};

pub struct ImageInfo {
    /// The magma image info associated with the `memory`.
    pub info: magma_image_info_t,

    /// The `BufferCollectionImportToken` associated with this file.
    pub token: Option<fuicomp::BufferCollectionImportToken>,
}

impl Clone for ImageInfo {
    fn clone(&self) -> Self {
        ImageInfo {
            info: self.info,
            token: self.token.as_ref().map(|token| fuicomp::BufferCollectionImportToken {
                value: fidl::EventPair::from_handle(
                    token
                        .value
                        .as_handle_ref()
                        .duplicate(zx::Rights::SAME_RIGHTS)
                        .expect("Failed to duplicate the buffer token."),
                ),
            }),
        }
    }
}

pub struct ImageFile {
    pub info: ImageInfo,

    pub memory: Arc<MemoryObject>,
}

impl ImageFile {
    pub fn new_file(
        current_task: &CurrentTask,
        info: ImageInfo,
        memory: MemoryObject,
    ) -> FileHandle {
        let memory_size = memory.get_size();

        let file = Anon::new_file_extended(
            current_task,
            Box::new(ImageFile { info, memory: Arc::new(memory) }),
            OpenFlags::RDWR,
            "[fuchsia:image]",
            |id| {
                let mut info =
                    FsNodeInfo::new(id, FileMode::from_bits(0o600), current_task.as_fscred());
                info.size = memory_size as usize;
                info
            },
        );

        file
    }
}

impl FileOps for ImageFile {
    fileops_impl_memory!(self, &self.memory);
    fileops_impl_noop_sync!();
}
