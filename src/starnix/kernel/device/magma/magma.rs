// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::vulkan::{BufferCollectionTokens, Loader};
use fidl_fuchsia_images2 as fimages2;
use fidl_fuchsia_sysmem2 as fsysmem2;
use fidl_fuchsia_ui_composition as fuicomp;
use fsysmem2::{AllocatorAllocateSharedCollectionRequest, BufferCollectionTokenDuplicateRequest};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_image_format::{
    constraints_to_image_format, drm_format_to_images2_format, drm_format_to_vulkan_format,
    drm_modifier_to_sysmem_modifier_2, get_plane_row_bytes_2, image_format_plane_byte_offset_2,
    sysmem_modifier_to_drm_modifier_2, DRM_FORMAT_MOD_INVALID,
};
use fuchsia_vulkan::{
    BufferCollectionConstraintsInfoFUCHSIA, ImageConstraintsInfoFUCHSIA,
    ImageFormatConstraintsInfoFUCHSIA, SysmemColorSpaceFUCHSIA,
    STRUCTURE_TYPE_BUFFER_COLLECTION_CONSTRAINTS_INFO_FUCHSIA,
    STRUCTURE_TYPE_IMAGE_CONSTRAINTS_INFO_FUCHSIA,
    STRUCTURE_TYPE_IMAGE_FORMAT_CONSTRAINTS_INFO_FUCHSIA,
    STRUCTURE_TYPE_SYSMEM_COLOR_SPACE_FUCHSIA,
};
use fuchsia_zircon as zx;
use magma::{
    magma_handle_t, magma_image_create_info_t, magma_image_info_t, magma_poll_item__bindgen_ty_1,
    magma_poll_item_t, magma_semaphore_t, magma_status_t, virtio_magma_ctrl_hdr_t,
    virtio_magma_ctrl_type, virtmagma_ioctl_args_magma_command, MAGMA_COHERENCY_DOMAIN_CPU,
    MAGMA_COHERENCY_DOMAIN_INACCESSIBLE, MAGMA_COHERENCY_DOMAIN_RAM,
    MAGMA_IMAGE_CREATE_FLAGS_PRESENTABLE, MAGMA_IMAGE_CREATE_FLAGS_VULKAN_USAGE,
    MAGMA_MAX_IMAGE_PLANES, MAGMA_POLL_TYPE_SEMAPHORE, MAGMA_STATUS_INTERNAL_ERROR,
    MAGMA_STATUS_INVALID_ARGS,
};
use starnix_core::{mm::MemoryAccessorExt, task::CurrentTask};
use starnix_logging::{log_warn, track_stub};
use starnix_uapi::{
    errno,
    errors::Errno,
    user_address::{UserAddress, UserRef},
};
use vk_sys as vk;
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

/// Reads a magma command and its type from user space.
///
/// # Parameters
/// - `current_task`: The task to which the command memory belongs.
/// - `command_address`: The address of the `virtmagma_ioctl_args_magma_command`.
pub fn read_magma_command_and_type(
    current_task: &CurrentTask,
    command_address: UserAddress,
) -> Result<(virtmagma_ioctl_args_magma_command, virtio_magma_ctrl_type), Errno> {
    let command: virtmagma_ioctl_args_magma_command =
        current_task.read_object(UserRef::new(command_address))?;

    let request_address = UserAddress::from(command.request_address);
    let header: virtio_magma_ctrl_hdr_t =
        current_task.read_object(UserRef::new(request_address))?;

    Ok((command, header.type_ as u16))
}

/// Reads the control and response structs from the given magma command struct.
///
/// # Parameters
/// - `current_task`: The task to which the memory belongs.
/// - `command`: The command struct that contains the pointers to the control and response structs.
pub fn read_control_and_response<C: Default + AsBytes + FromBytes, R: Default>(
    current_task: &CurrentTask,
    command: &virtmagma_ioctl_args_magma_command,
) -> Result<(C, R), Errno> {
    let request_address = UserAddress::from(command.request_address);
    let ctrl = current_task.read_object(UserRef::new(request_address))?;

    Ok((ctrl, R::default()))
}

/// Creates an image in a buffer collection.
///
/// # Parameters
/// - `physical_device_index`: The index of the physical device to use when initializing the Vulkan
///                            loader.
/// - `create_info`: The magma info used to create the image.
///
/// Returns the image vmo, an import token for the collection, and the image info for the created
/// image.
pub fn create_drm_image(
    physical_device_index: u32,
    create_info: &magma_image_create_info_t,
) -> Result<
    (zx::Vmo, Option<fuicomp::BufferCollectionImportToken>, magma_image_info_t),
    magma_status_t,
> {
    let flags = create_info.flags as u32;
    const SUPPORTED_FLAGS: u32 =
        MAGMA_IMAGE_CREATE_FLAGS_VULKAN_USAGE | MAGMA_IMAGE_CREATE_FLAGS_PRESENTABLE;
    if (flags & !SUPPORTED_FLAGS) != 0 {
        return Err(MAGMA_STATUS_INVALID_ARGS);
    }
    let use_scenic = (flags & MAGMA_IMAGE_CREATE_FLAGS_PRESENTABLE) != 0;

    let vk_format = drm_format_to_vulkan_format(create_info.drm_format as u32)
        .map_err(|_| MAGMA_STATUS_INVALID_ARGS)?;

    let sysmem_format = drm_format_to_images2_format(create_info.drm_format as u32)
        .map_err(|_| MAGMA_STATUS_INVALID_ARGS)?;

    let mut sysmem_modifiers: Vec<fimages2::PixelFormatModifier> = vec![];
    let mut terminator_found = false;
    for modifier in create_info.drm_format_modifiers {
        if modifier == DRM_FORMAT_MOD_INVALID {
            terminator_found = true;
            break;
        }

        let modifier =
            drm_modifier_to_sysmem_modifier_2(modifier).map_err(|_| MAGMA_STATUS_INVALID_ARGS)?;

        sysmem_modifiers.push(modifier);
    }

    if !terminator_found {
        return Err(MAGMA_STATUS_INVALID_ARGS);
    }

    let loader = Loader::new(physical_device_index).map_err(|_| MAGMA_STATUS_INTERNAL_ERROR)?;

    let mut vk_format_features = 0 as vk::FormatFeatureFlagBits;
    let mut vk_usage = 0 as vk::ImageUsageFlagBits;

    if (flags & MAGMA_IMAGE_CREATE_FLAGS_VULKAN_USAGE) != 0 {
        // Use the Vulkan usage as provided by the client.
        vk_usage = (create_info.flags >> 32) as vk::ImageUsageFlagBits;

        if (vk_usage & vk::IMAGE_USAGE_TRANSFER_SRC_BIT) != 0 {
            vk_format_features |= vk::FORMAT_FEATURE_TRANSFER_SRC_BIT_KHR;
        }
        if (vk_usage & vk::IMAGE_USAGE_TRANSFER_DST_BIT) != 0 {
            vk_format_features |= vk::FORMAT_FEATURE_TRANSFER_DST_BIT_KHR;
        }
        if (vk_usage & vk::IMAGE_USAGE_SAMPLED_BIT) != 0 {
            vk_format_features |= vk::FORMAT_FEATURE_SAMPLED_IMAGE_BIT;
        }
        if (vk_usage & vk::IMAGE_USAGE_STORAGE_BIT) != 0 {
            vk_format_features |= vk::FORMAT_FEATURE_STORAGE_IMAGE_BIT;
        }
        if (vk_usage & vk::IMAGE_USAGE_COLOR_ATTACHMENT_BIT) != 0 {
            vk_format_features |= vk::FORMAT_FEATURE_COLOR_ATTACHMENT_BIT;
        }
        if (vk_usage & vk::IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT) != 0 {
            vk_format_features |= vk::FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT;
        }
    } else {
        // If linear isn't requested, assume we'll get a tiled format modifier.
        let is_linear_tiling = sysmem_modifiers.len() == 1
            && sysmem_modifiers[0] == fimages2::PixelFormatModifier::Linear;

        vk_format_features = loader.get_format_features(vk_format, is_linear_tiling);

        // For non-ICD clients like GBM, the client API has no fine grained usage.
        // To maximize compatibility, we pass as many usages as make sense given the format features.
        if (vk_format_features & vk::FORMAT_FEATURE_TRANSFER_SRC_BIT_KHR) != 0 {
            vk_usage |= vk::IMAGE_USAGE_TRANSFER_SRC_BIT;
        }
        if (vk_format_features & vk::FORMAT_FEATURE_TRANSFER_DST_BIT_KHR) != 0 {
            vk_usage |= vk::IMAGE_USAGE_TRANSFER_DST_BIT;
        }
        if (vk_format_features & vk::FORMAT_FEATURE_SAMPLED_IMAGE_BIT) != 0 {
            vk_usage |= vk::IMAGE_USAGE_SAMPLED_BIT;
        }
        // Intel Vulkan driver doesn't support CCS with storage images; assume the client doesn't need
        // storage to allow for the performance benefit of CCS.
        if !loader.is_intel_device()
            && ((vk_format_features & vk::FORMAT_FEATURE_STORAGE_IMAGE_BIT) != 0)
        {
            vk_usage |= vk::IMAGE_USAGE_STORAGE_BIT;
        }
        if (vk_format_features & vk::FORMAT_FEATURE_COLOR_ATTACHMENT_BIT) != 0 {
            vk_usage |= vk::IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
            vk_usage |= vk::IMAGE_USAGE_INPUT_ATTACHMENT_BIT;
        }
        if (vk_format_features & vk::FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT) != 0 {
            vk_usage |= vk::IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT;
            vk_usage |= vk::IMAGE_USAGE_INPUT_ATTACHMENT_BIT;
        }
        // No format features apply here.
        vk_usage |= vk::IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT;
    };

    track_stub!(TODO("https://fxbug.dev/322873633"), "magma verify physical device limits");
    let scenic_allocator = if use_scenic {
        Some(init_scenic().map_err(|_| MAGMA_STATUS_INTERNAL_ERROR)?)
    } else {
        None
    };

    let image_create_info = vk::ImageCreateInfo {
        sType: vk::STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        pNext: std::ptr::null(),
        flags: 0,
        imageType: vk::IMAGE_TYPE_2D,
        format: vk_format,
        extent: vk::Extent3D { width: create_info.width, height: create_info.height, depth: 1 },
        mipLevels: 1,
        arrayLayers: 1,
        samples: vk::SAMPLE_COUNT_1_BIT,
        tiling: vk::IMAGE_TILING_OPTIMAL,
        usage: vk_usage,
        sharingMode: vk::SHARING_MODE_EXCLUSIVE,
        queueFamilyIndexCount: 0,
        pQueueFamilyIndices: std::ptr::null(),
        initialLayout: vk::IMAGE_LAYOUT_UNDEFINED,
    };

    let rgb_color_space = SysmemColorSpaceFUCHSIA {
        sType: STRUCTURE_TYPE_SYSMEM_COLOR_SPACE_FUCHSIA,
        pNext: std::ptr::null(),
        colorSpace: fimages2::ColorSpace::Srgb.into_primitive(),
    };

    let format_info = ImageFormatConstraintsInfoFUCHSIA {
        sType: STRUCTURE_TYPE_IMAGE_FORMAT_CONSTRAINTS_INFO_FUCHSIA,
        pNext: std::ptr::null(),
        imageCreateInfo: image_create_info,
        requiredFormatFeatures: vk_format_features,
        flags: 0,
        sysmemPixelFormat: 0,
        colorSpaceCount: 1,
        pColorSpaces: &rgb_color_space,
    };

    let image_constraints = ImageConstraintsInfoFUCHSIA {
        sType: STRUCTURE_TYPE_IMAGE_CONSTRAINTS_INFO_FUCHSIA,
        pNext: std::ptr::null(),
        formatConstraintsCount: 1,
        pFormatConstraints: &format_info,
        bufferCollectionConstraints: BufferCollectionConstraintsInfoFUCHSIA {
            sType: STRUCTURE_TYPE_BUFFER_COLLECTION_CONSTRAINTS_INFO_FUCHSIA,
            pNext: std::ptr::null(),
            minBufferCount: 1,
            maxBufferCount: 1,
            minBufferCountForCamping: 0,
            minBufferCountForDedicatedSlack: 0,
            minBufferCountForSharedSlack: 0,
        },
        flags: 0,
    };

    let (tokens, sysmem_allocator) =
        init_sysmem(use_scenic).map_err(|_| MAGMA_STATUS_INTERNAL_ERROR)?;

    let (scenic_import_token, buffer_collection) = loader
        .create_collection(
            vk::Extent2D { width: create_info.width, height: create_info.height },
            &image_constraints,
            sysmem_format,
            &sysmem_modifiers,
            tokens,
            &scenic_allocator,
            &sysmem_allocator,
        )
        .map_err(|_| MAGMA_STATUS_INVALID_ARGS)?;

    let (vmo, image_info) =
        get_image_info(buffer_collection, create_info.width, create_info.height)
            .map_err(|_| MAGMA_STATUS_INVALID_ARGS)?;

    Ok((vmo, scenic_import_token, image_info))
}

/// Initializes and returns Scenic allocator proxy.
pub fn init_scenic() -> Result<fuicomp::AllocatorSynchronousProxy, Errno> {
    Ok(connect_to_protocol_sync::<fuicomp::AllocatorMarker>().map_err(|_| errno!(ENOENT))?)
}

/// Allocates a shared sysmem collection.
///
/// The returned `BufferCollectionTokens` contains a proxy to the shared collection, as well as a
/// duplicate token to use for both Scenic and Vulkan.
pub fn init_sysmem(
    use_scenic: bool,
) -> Result<(BufferCollectionTokens, fsysmem2::AllocatorSynchronousProxy), Errno> {
    let sysmem_allocator =
        connect_to_protocol_sync::<fsysmem2::AllocatorMarker>().map_err(|_| errno!(ENOENT))?;

    let (client, remote) =
        fidl::endpoints::create_endpoints::<fsysmem2::BufferCollectionTokenMarker>();

    sysmem_allocator
        .allocate_shared_collection(AllocatorAllocateSharedCollectionRequest {
            token_request: Some(remote),
            ..Default::default()
        })
        .map_err(|_| errno!(EINVAL))?;

    let buffer_token_proxy =
        fsysmem2::BufferCollectionTokenSynchronousProxy::new(client.into_channel());

    let scenic_token = if use_scenic {
        let (token, remote) =
            fidl::endpoints::create_endpoints::<fsysmem2::BufferCollectionTokenMarker>();

        buffer_token_proxy
            .duplicate(BufferCollectionTokenDuplicateRequest {
                rights_attenuation_mask: Some(fidl::Rights::SAME_RIGHTS),
                token_request: Some(remote),
                ..Default::default()
            })
            .map_err(|_| errno!(EINVAL))?;
        Some(token)
    } else {
        None
    };

    let (vulkan_token, remote) =
        fidl::endpoints::create_endpoints::<fsysmem2::BufferCollectionTokenMarker>();

    buffer_token_proxy
        .duplicate(BufferCollectionTokenDuplicateRequest {
            rights_attenuation_mask: Some(fidl::Rights::SAME_RIGHTS),
            token_request: Some(remote),
            ..Default::default()
        })
        .map_err(|_| errno!(EINVAL))?;

    buffer_token_proxy.sync(zx::Time::INFINITE).map_err(|_| errno!(EINVAL))?;

    Ok((
        BufferCollectionTokens { buffer_token_proxy, scenic_token, vulkan_token },
        sysmem_allocator,
    ))
}

/// Waits for buffers to be allocated in the provided buffer collection and returns the first buffer
/// in the collection, as well as the image info for the buffer.
///
/// # Parameters
/// - `buffer_collection`: The collection to fetch the image and info from.
/// - `width`: The width to use when creating the image format.
/// - `height`: The height to use when creating the image format.
pub fn get_image_info(
    buffer_collection: fsysmem2::BufferCollectionSynchronousProxy,
    width: u32,
    height: u32,
) -> Result<(zx::Vmo, magma_image_info_t), Errno> {
    let mut collection_info = buffer_collection
        .wait_for_all_buffers_allocated(zx::Time::INFINITE)
        .map_err(|err| {
            log_warn!("wait_for_all_buffers_allocated failed (fidl layer): {}", err);
            errno!(EINVAL)
        })?
        .map_err(|err| {
            log_warn!("wait_for_all_buffers_allocated failed (app layer): {:?}", err);
            errno!(EINVAL)
        })?
        .buffer_collection_info
        .expect("buffer_colllection_info");
    let _ = buffer_collection.release();

    let image_format = constraints_to_image_format(
        collection_info.settings.as_ref().unwrap().image_format_constraints.as_ref().unwrap(),
        width,
        height,
    )
    .map_err(|_| errno!(EINVAL))?;

    let mut image_info = magma_image_info_t::default();
    for plane in 0..MAGMA_MAX_IMAGE_PLANES {
        image_info.plane_offsets[plane as usize] =
            image_format_plane_byte_offset_2(&image_format, plane).unwrap_or(0);
        image_info.plane_strides[plane as usize] =
            get_plane_row_bytes_2(&image_format, plane).unwrap_or(0) as u64;
    }

    image_info.drm_format_modifier = sysmem_modifier_to_drm_modifier_2(
        *image_format.pixel_format_modifier.as_ref().expect("format_modifier"),
    )
    .unwrap_or(0);
    image_info.coherency_domain = match collection_info
        .settings
        .as_ref()
        .expect("settings")
        .buffer_settings
        .as_ref()
        .expect("buffer_settings")
        .coherency_domain
        .as_ref()
        .expect("coherency_domain")
    {
        fsysmem2::CoherencyDomain::Cpu => MAGMA_COHERENCY_DOMAIN_CPU,
        fsysmem2::CoherencyDomain::Ram => MAGMA_COHERENCY_DOMAIN_RAM,
        fsysmem2::CoherencyDomain::Inaccessible => MAGMA_COHERENCY_DOMAIN_INACCESSIBLE,
        _ => return Err(errno!(EINVAL)),
    };

    let vmo = collection_info.buffers.as_mut().expect("buffers")[0]
        .vmo
        .take()
        .ok_or_else(|| errno!(EINVAL))?;
    Ok((vmo, image_info))
}

#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Copy, Clone, Default, Debug)]
/// `StarnixPollItem` exists to be able to `AsBytes` and `FromBytes` the union that exists in
/// `magma_poll_item_t`.
pub struct StarnixPollItem {
    pub semaphore_or_handle: u64,
    pub type_: u32,
    pub condition: u32,
    pub result: u32,
    pub unused: u32,
}

impl StarnixPollItem {
    pub fn new(poll_item: &magma_poll_item_t) -> StarnixPollItem {
        let semaphore_or_handle = unsafe {
            if poll_item.type_ == MAGMA_POLL_TYPE_SEMAPHORE {
                poll_item.__bindgen_anon_1.semaphore
            } else {
                poll_item.__bindgen_anon_1.handle as u64
            }
        };
        StarnixPollItem {
            semaphore_or_handle,
            type_: poll_item.type_,
            condition: poll_item.condition,
            result: poll_item.result,
            unused: 0,
        }
    }

    pub fn as_poll_item(&self) -> magma_poll_item_t {
        let handle = if self.type_ == MAGMA_POLL_TYPE_SEMAPHORE {
            magma_poll_item__bindgen_ty_1 {
                semaphore: self.semaphore_or_handle as magma_semaphore_t,
            }
        } else {
            magma_poll_item__bindgen_ty_1 { handle: self.semaphore_or_handle as magma_handle_t }
        };
        magma_poll_item_t {
            __bindgen_anon_1: handle,
            type_: self.type_,
            condition: self.condition,
            result: self.result,
            unused: self.unused,
        }
    }
}
