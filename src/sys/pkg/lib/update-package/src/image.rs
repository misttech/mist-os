// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {fuchsia_zircon_status::Status, thiserror::Error};

#[cfg(target_os = "fuchsia")]
use {fidl_fuchsia_io as fio, fidl_fuchsia_mem as fmem, fuchsia_zircon::VmoChildOptions};

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

/// An identifier for an image type which corresponds to the file's name without
/// a subtype.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum ImageType {
    /// Kernel image.
    Zbi,

    /// Metadata for the kernel image.
    FuchsiaVbmeta,

    /// Recovery image.
    Recovery,

    /// Metadata for recovery image.
    RecoveryVbmeta,

    /// Firmware
    Firmware,
}

impl ImageType {
    /// The name of the ImageType.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Zbi => "zbi",
            Self::FuchsiaVbmeta => "fuchsia.vbmeta",
            Self::Recovery => "recovery",
            Self::RecoveryVbmeta => "recovery.vbmeta",
            Self::Firmware => "firmware",
        }
    }
}

/// An identifier for an image that can be paved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    imagetype: ImageType,
    filename: String,
}

impl Image {
    /// Construct an Image using the given imagetype and optional subtype.
    pub fn new(imagetype: ImageType, subtype: Option<&str>) -> Self {
        let filename = match subtype {
            None => imagetype.name().to_string(),
            Some(subtype) => format!("{}_{}", imagetype.name(), subtype),
        };
        Self { imagetype, filename }
    }

    /// The imagetype of the image relative to the update package.
    pub fn imagetype(&self) -> ImageType {
        self.imagetype
    }

    /// The particular type of this image as understood by the paver service, if present.
    pub fn subtype(&self) -> Option<&str> {
        if self.filename.len() == self.imagetype.name().len() {
            return None;
        }

        Some(&self.filename[(self.imagetype.name().len() + 1)..])
    }
}

impl ImageType {
    /// Determines if this image type would target a recovery partition.
    pub fn targets_recovery(self) -> bool {
        match self {
            Self::Recovery | Self::RecoveryVbmeta => true,
            Self::Zbi | Self::FuchsiaVbmeta | Self::Firmware => false,
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
            VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE | VmoChildOptions::RESIZABLE,
            0,
            size,
        )
        .map_err(|status| OpenImageError::CloneBuffer { path: path.to_string(), status })?;

    Ok(fmem::Buffer { vmo, size })
}

#[cfg(test)]
mod tests {
    use {super::*, proptest::prelude::*, proptest_derive::Arbitrary};

    #[test]
    fn image_new() {
        assert_eq!(
            Image::new(ImageType::Zbi, None),
            Image { imagetype: ImageType::Zbi, filename: "zbi".to_string() }
        );
    }

    #[test]
    fn recovery_images_target_recovery() {
        assert!(
            Image::new(ImageType::Recovery, None).imagetype().targets_recovery(),
            "image recovery should target recovery",
        );
        assert!(
            Image::new(ImageType::RecoveryVbmeta, None).imagetype().targets_recovery(),
            "image recovery.vbmeta should target recovery",
        );
    }

    #[test]
    fn non_recovery_images_do_not_target_recovery() {
        assert!(
            !Image::new(ImageType::Zbi, None).imagetype().targets_recovery(),
            "image zbi should not target recovery",
        );
        assert!(
            !Image::new(ImageType::FuchsiaVbmeta, None).imagetype().targets_recovery(),
            "image fuchsia.vbmeta should not target recovery",
        );
        assert!(
            !Image::new(ImageType::Firmware, None).imagetype().targets_recovery(),
            "image firmware should not target recovery",
        );
    }

    #[test]
    fn test_image_typed_accessors() {
        let image = Image::new(ImageType::Zbi, None);
        assert_eq!(image.imagetype(), ImageType::Zbi);
        assert_eq!(image.subtype(), None);

        let image = Image::new(ImageType::Zbi, Some("ibz"));
        assert_eq!(image.imagetype(), ImageType::Zbi);
        assert_eq!(image.subtype(), Some("ibz"));
    }

    #[derive(Debug, Arbitrary)]
    enum ImageConstructor {
        New,
        MatchesBase,
    }

    prop_compose! {
        fn arb_image()(
            constructor: ImageConstructor,
            imagetype: ImageType,
            subtype: Option<String>,
        ) -> Image {
            let subtype = subtype.as_deref();
            let image = Image::new(imagetype, subtype);

            match constructor {
                ImageConstructor::New => image,
                ImageConstructor::MatchesBase => {
                    Image::new(imagetype, None)
                }
            }
        }
    }

    proptest! {
        #[test]
        fn image_accessors_do_not_panic(image in arb_image()) {
            image.subtype();
            image.imagetype();
            format!("{image:?}");
        }
    }
}
