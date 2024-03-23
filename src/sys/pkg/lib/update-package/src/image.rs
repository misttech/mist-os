// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {fuchsia_zircon_status::Status, thiserror::Error};

#[cfg(target_os = "fuchsia")]
use {fidl_fuchsia_io as fio, fidl_fuchsia_mem as fmem, fuchsia_zircon as zx};

/// An error encountered while opening an image.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum OpenImageError {
    #[error("while opening the file path {path:?}")]
    OpenPath {
        path: String,
        #[source]
        err: fuchsia_fs::node::OpenError,
    },

    #[error("while calling get_backing_memory for {path:?}")]
    FidlGetBackingMemory {
        path: String,
        #[source]
        err: fidl::Error,
    },

    #[error("while obtaining vmo of file for {path:?}: {status}")]
    GetBackingMemory { path: String, status: Status },

    #[error("while converting vmo to a resizable vmo for {path:?}: {status}")]
    CloneBuffer { path: String, status: Status },
}

/// An identifier for an image that can be paved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// Kernel image.
    Zbi,

    /// Metadata for the kernel image.
    FuchsiaVbmeta,

    /// Recovery image.
    Recovery,

    /// Metadata for recovery image.
    RecoveryVbmeta,

    /// Firmware
    Firmware {
        /// A device-specific string given to the paver when writing the image.
        /// Frequently the empty string on devices that only support a single firmware image type
        /// (the bootloader).
        /// https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.paver/paver.fidl;l=173-175;drc=4902eb163d5036cd2d2889b6cb22cb42a1cdd6b5
        type_: String,
    },
}

impl Image {
    /// Determines if this image would target a recovery partition.
    pub fn targets_recovery(self) -> bool {
        match self {
            Self::Recovery | Self::RecoveryVbmeta => true,
            Self::Zbi | Self::FuchsiaVbmeta | Self::Firmware { .. } => false,
        }
    }
}

#[cfg(target_os = "fuchsia")]
/// Opens the given `path` as a resizable VMO buffer and returns the buffer on success.
pub(crate) async fn open_from_path(
    proxy: &fio::DirectoryProxy,
    path: &str,
) -> Result<fmem::Buffer, OpenImageError> {
    let file = fuchsia_fs::directory::open_file(proxy, path, fio::OpenFlags::RIGHT_READABLE)
        .await
        .map_err(|err| OpenImageError::OpenPath { path: path.to_string(), err })?;

    let vmo = file
        .get_backing_memory(fio::VmoFlags::READ)
        .await
        .map_err(|err| OpenImageError::FidlGetBackingMemory { path: path.to_string(), err })?
        .map_err(Status::from_raw)
        .map_err(|status| OpenImageError::GetBackingMemory { path: path.to_string(), status })?;

    let size = vmo
        .get_content_size()
        .map_err(|status| OpenImageError::GetBackingMemory { path: path.to_string(), status })?;

    // The paver service requires VMOs that are resizable, and blobfs does not give out resizable
    // VMOs. Fortunately, a copy-on-write child clone of the vmo can be made resizable.
    let vmo = vmo
        .create_child(
            zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE | zx::VmoChildOptions::RESIZABLE,
            0,
            size,
        )
        .map_err(|status| OpenImageError::CloneBuffer { path: path.to_string(), status })?;

    Ok(fmem::Buffer { vmo, size })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_images_target_recovery() {
        assert!(Image::Recovery.targets_recovery(), "image recovery should target recovery",);
        assert!(
            Image::RecoveryVbmeta.targets_recovery(),
            "image recovery.vbmeta should target recovery",
        );
    }

    #[test]
    fn non_recovery_images_do_not_target_recovery() {
        assert!(!Image::Zbi.targets_recovery(), "image zbi should not target recovery",);
        assert!(
            !Image::FuchsiaVbmeta.targets_recovery(),
            "image fuchsia.vbmeta should not target recovery",
        );
        assert!(
            !Image::Firmware { type_: "".into() }.targets_recovery(),
            "image firmware should not target recovery",
        );
    }
}
