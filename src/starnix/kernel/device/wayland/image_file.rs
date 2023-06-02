// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_ui_composition as fuicomp;
use fuchsia_zircon as zx;
use fuchsia_zircon::{AsHandleRef, HandleBased};
use magma::*;

use std::sync::Arc;

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::mm::ProtectionFlags;
use crate::task::CurrentTask;
use crate::types::*;

pub struct ImageInfo {
    /// The magma image info associated with the `vmo`.
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

    pub vmo: Arc<zx::Vmo>,
}

impl ImageFile {
    pub fn new_file(current_task: &CurrentTask, info: ImageInfo, vmo: zx::Vmo) -> FileHandle {
        let vmo_size = vmo.get_size().unwrap();

        let file = Anon::new_file_extended(
            current_task.kernel(),
            Box::new(ImageFile { info, vmo: Arc::new(vmo) }),
            OpenFlags::RDWR,
            |id| {
                let mut info =
                    FsNodeInfo::new(id, FileMode::from_bits(0o600), current_task.as_fscred());
                info.size = vmo_size as usize;
                info
            },
        );

        file
    }
}

impl FileOps for ImageFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileObject::read(&self.vmo, file, offset, data)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileObject::write(&self.vmo, file, current_task, offset, data, None)
    }

    fn get_vmo(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        VmoFileObject::get_vmo(&self.vmo, file, current_task, prot)
    }
}
