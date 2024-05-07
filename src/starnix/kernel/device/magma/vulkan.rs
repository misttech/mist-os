// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ClientEnd;
use fidl_fuchsia_images2 as fimages2;
use fidl_fuchsia_math::SizeU;
use fidl_fuchsia_sysmem2 as fsysmem2;
use fidl_fuchsia_ui_composition as fuicomp;
use fsysmem2::{AllocatorBindSharedCollectionRequest, BufferCollectionSetConstraintsRequest};
use fuchsia_vulkan::{
    device_pointers, entry_points, instance_pointers, BufferCollectionCreateInfoFUCHSIA,
    BufferCollectionFUCHSIA, FuchsiaExtensionPointers, ImageConstraintsInfoFUCHSIA,
    STRUCTURE_TYPE_BUFFER_COLLECTION_CREATE_INFO_FUCHSIA,
};
use fuchsia_zircon as zx;
use fuchsia_zircon::AsHandleRef;
use std::mem;
use vk_sys as vk;

/// `BufferCollectionTokens` contains all the buffer collection tokens required to initialize a
/// `Loader`.
///
/// The `buffer_token_proxy` is expected to be the source (via `duplicate`) of both the
/// `scenic_token` as well as the `vulkan_token`.
pub struct BufferCollectionTokens {
    /// The buffer collection token that is the source of the scenic and vulkan tokens.
    pub buffer_token_proxy: fsysmem2::BufferCollectionTokenSynchronousProxy,

    /// The buffer collection token that is handed off to scenic.
    pub scenic_token: Option<ClientEnd<fsysmem2::BufferCollectionTokenMarker>>,

    /// The buffer collection token that is handed off to vulkan.
    pub vulkan_token: ClientEnd<fsysmem2::BufferCollectionTokenMarker>,
}

/// `Loader` stores all the interfaces/entry points for the created devices and instances.
pub struct Loader {
    /// The vulkan instance pointers associated with this loader. These are fetched at loader
    /// creation time.
    instance_pointers: vk::InstancePointers,

    /// The instance that is created for this loader. Not currently used after the loader has been
    /// created.
    _instance: vk::Instance,

    /// The physical device associated with this loader. Not currently used after the loader has
    /// been created.
    _physical_device: vk::PhysicalDevice,

    /// The device that is created by this Loader.
    device: vk::Device,
}

impl Drop for Loader {
    fn drop(&mut self) {
        let vk_d = device_pointers(&self.instance_pointers, self.device);
        unsafe {
            vk_d.DestroyDevice(self.device, std::ptr::null());
            self.instance_pointers.DestroyInstance(self._instance, std::ptr::null());
        }
    }
}

impl Loader {
    /// Creates a new `Loader` by creating a Vulkan instance and device, with the provided physical
    /// device index.
    pub fn new(physical_device_index: u32) -> Result<Loader, vk::Result> {
        let entry_points = entry_points();
        let application_info = app_info();
        let instance_info = instance_info(&application_info);

        let mut instance: usize = 0;
        let result = unsafe {
            entry_points.CreateInstance(
                &instance_info,
                std::ptr::null(),
                &mut instance as *mut vk::Instance,
            )
        };
        if result != vk::SUCCESS {
            return Err(result);
        }

        let instance_pointers = instance_pointers(instance);
        let mut device_count: u32 = 0;
        if unsafe {
            instance_pointers.EnumeratePhysicalDevices(
                instance,
                &mut device_count as *mut u32,
                std::ptr::null_mut(),
            )
        } != vk::SUCCESS
        {
            return Err(vk::ERROR_INITIALIZATION_FAILED);
        }

        let mut physical_devices: Vec<vk::PhysicalDevice> = vec![0; device_count as usize];
        if unsafe {
            instance_pointers.EnumeratePhysicalDevices(
                instance,
                &mut device_count as *mut u32,
                physical_devices.as_mut_ptr(),
            )
        } != vk::SUCCESS
        {
            return Err(vk::ERROR_INITIALIZATION_FAILED);
        }

        let physical_device = physical_devices[physical_device_index as usize];

        let mut queue_family_count: u32 = 0;
        unsafe {
            instance_pointers.GetPhysicalDeviceQueueFamilyProperties(
                physical_device,
                &mut queue_family_count as *mut u32,
                std::ptr::null_mut(),
            );
        };

        let mut queue_family_properties =
            Vec::<vk::QueueFamilyProperties>::with_capacity(queue_family_count as usize);
        queue_family_properties.resize_with(queue_family_count as usize, || {
            vk::QueueFamilyProperties {
                queueFlags: 0,
                queueCount: 0,
                timestampValidBits: 0,
                minImageTransferGranularity: vk::Extent3D { width: 0, height: 0, depth: 0 },
            }
        });
        unsafe {
            instance_pointers.GetPhysicalDeviceQueueFamilyProperties(
                physical_device,
                &mut queue_family_count as *mut u32,
                queue_family_properties.as_mut_ptr(),
            );
        };

        let mut queue_family_index = None;
        for (index, queue_family) in queue_family_properties.iter().enumerate() {
            if queue_family.queueFlags & vk::QUEUE_GRAPHICS_BIT != 0 {
                queue_family_index = Some(index);
            }
        }
        if queue_family_index.is_none() {
            return Err(vk::ERROR_INITIALIZATION_FAILED);
        }

        let queue_family_index = queue_family_index.unwrap();
        let queue_create_info = vk::DeviceQueueCreateInfo {
            sType: vk::STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
            pNext: std::ptr::null(),
            flags: 0,
            queueFamilyIndex: queue_family_index as u32,
            queueCount: 1,
            pQueuePriorities: &0.0,
        };

        let extension_names = [c"VK_FUCHSIA_buffer_collection".as_ptr()];

        let device_create_info = vk::DeviceCreateInfo {
            sType: vk::STRUCTURE_TYPE_DEVICE_CREATE_INFO,
            pNext: std::ptr::null(),
            flags: 0,
            queueCreateInfoCount: 1,
            pQueueCreateInfos: &queue_create_info as *const vk::DeviceQueueCreateInfo,
            enabledLayerCount: 0,
            ppEnabledLayerNames: std::ptr::null(),
            enabledExtensionCount: extension_names.len() as u32,
            ppEnabledExtensionNames: extension_names.as_ptr(),
            pEnabledFeatures: std::ptr::null(),
        };

        let mut device: usize = 0;
        let result = unsafe {
            instance_pointers.CreateDevice(
                physical_device,
                &device_create_info as *const vk::DeviceCreateInfo,
                std::ptr::null(),
                &mut device as *mut usize,
            )
        };
        if result != vk::SUCCESS {
            return Err(result);
        }

        Ok(Loader {
            instance_pointers,
            _instance: instance,
            _physical_device: physical_device,
            device,
        })
    }

    pub fn is_intel_device(&self) -> bool {
        unsafe {
            let mut props: vk::PhysicalDeviceProperties = mem::zeroed();
            self.instance_pointers.GetPhysicalDeviceProperties(self._physical_device, &mut props);
            props.vendorID == 0x8086
        }
    }

    pub fn get_format_features(
        &self,
        format: vk::Format,
        linear_tiling: bool,
    ) -> vk::FormatFeatureFlags {
        let mut format_properties = vk::FormatProperties {
            linearTilingFeatures: 0,
            optimalTilingFeatures: 0,
            bufferFeatures: 0,
        };
        unsafe {
            self.instance_pointers.GetPhysicalDeviceFormatProperties(
                self._physical_device,
                format,
                &mut format_properties,
            );
        }
        if linear_tiling {
            format_properties.linearTilingFeatures
        } else {
            format_properties.optimalTilingFeatures
        }
    }

    /// Creates a new `BufferCollection` and returns the image import endpoint for the collection,
    /// as well as an active proxy to the collection.
    ///
    /// Returns an error if creating the collection fails.
    pub fn create_collection(
        &self,
        extent: vk::Extent2D,
        image_constraints_info: &ImageConstraintsInfoFUCHSIA,
        pixel_format: fimages2::PixelFormat,
        modifiers: &[fimages2::PixelFormatModifier],
        tokens: BufferCollectionTokens,
        scenic_allocator: &Option<fuicomp::AllocatorSynchronousProxy>,
        sysmem_allocator: &fsysmem2::AllocatorSynchronousProxy,
    ) -> Result<
        (Option<fuicomp::BufferCollectionImportToken>, fsysmem2::BufferCollectionSynchronousProxy),
        vk::Result,
    > {
        let scenic_import_token = if let Some(allocator) = &scenic_allocator {
            Some(register_buffer_collection_with_scenic(
                tokens.scenic_token.ok_or(vk::ERROR_INITIALIZATION_FAILED)?,
                allocator,
            )?)
        } else {
            None
        };

        let vk_ext = FuchsiaExtensionPointers::load(|name| unsafe {
            self.instance_pointers.GetDeviceProcAddr(self.device, name.as_ptr()) as *const _
        });

        let mut vk_collection: BufferCollectionFUCHSIA = 0;
        let result = unsafe {
            vk_ext.CreateBufferCollectionFUCHSIA(
                self.device,
                &BufferCollectionCreateInfoFUCHSIA {
                    sType: STRUCTURE_TYPE_BUFFER_COLLECTION_CREATE_INFO_FUCHSIA,
                    pNext: std::ptr::null(),
                    collectionToken: tokens.vulkan_token.as_handle_ref().raw_handle(),
                },
                std::ptr::null(),
                &mut vk_collection as *mut BufferCollectionFUCHSIA,
            )
        };
        if result != vk::SUCCESS {
            return Err(result);
        }
        assert!(vk_collection != 0);

        let result = unsafe {
            vk_ext.SetBufferCollectionImageConstraintsFUCHSIA(
                self.device,
                vk_collection,
                image_constraints_info as *const ImageConstraintsInfoFUCHSIA,
            )
        };

        unsafe {
            vk_ext.DestroyBufferCollectionFUCHSIA(self.device, vk_collection, std::ptr::null());
        }

        if result != vk::SUCCESS {
            return Err(result);
        }

        let (client, remote) =
            fidl::endpoints::create_endpoints::<fsysmem2::BufferCollectionMarker>();

        sysmem_allocator
            .bind_shared_collection(AllocatorBindSharedCollectionRequest {
                token: Some(tokens.buffer_token_proxy.into_channel().into()),
                buffer_collection_request: Some(remote),
                ..Default::default()
            })
            .map_err(|_| vk::ERROR_INITIALIZATION_FAILED)?;
        let buffer_collection = client.into_sync_proxy();

        let mut constraints = buffer_collection_constraints();

        constraints.image_format_constraints = Some(
            modifiers
                .iter()
                .map(|modifier| fsysmem2::ImageFormatConstraints {
                    pixel_format: Some(pixel_format),
                    pixel_format_modifier: Some(*modifier),
                    min_size: Some(SizeU { width: extent.width, height: extent.height }),
                    max_size: Some(SizeU { width: extent.width, height: extent.height }),
                    color_spaces: Some(vec![fimages2::ColorSpace::Srgb]),
                    ..Default::default()
                })
                .collect(),
        );

        buffer_collection
            .set_constraints(BufferCollectionSetConstraintsRequest {
                constraints: Some(constraints),
                ..Default::default()
            })
            .map_err(|_| vk::ERROR_INITIALIZATION_FAILED)?;

        Ok((scenic_import_token, buffer_collection))
    }
}

/// Returns a `u32` encoding the provided vulkan version.
macro_rules! vulkan_version {
    ( $major:expr, $minor:expr, $patch:expr ) => {
        ($major as u32) << 22 | ($minor as u32) << 12 | ($patch as u32)
    };
}

/// Returns the default buffer collection constraints set on the buffer collection.
///
/// The returned buffer collection constraints are modified by the caller to contain the appropriate
/// image format constraints before being set on the collection.
pub fn buffer_collection_constraints() -> fsysmem2::BufferCollectionConstraints {
    let usage = fsysmem2::BufferUsage {
        cpu: Some(fsysmem2::CPU_USAGE_READ_OFTEN | fsysmem2::CPU_USAGE_WRITE_OFTEN),
        ..Default::default()
    };

    let buffer_memory_constraints = fsysmem2::BufferMemoryConstraints {
        ram_domain_supported: Some(true),
        cpu_domain_supported: Some(true),
        ..Default::default()
    };

    fsysmem2::BufferCollectionConstraints {
        min_buffer_count: Some(1),
        usage: Some(usage),
        buffer_memory_constraints: Some(buffer_memory_constraints),
        ..Default::default()
    }
}

/// Returns the Vulkan application info for starnix.
pub fn app_info() -> vk::ApplicationInfo {
    vk::ApplicationInfo {
        sType: vk::STRUCTURE_TYPE_APPLICATION_INFO,
        pNext: std::ptr::null(),
        pApplicationName: c"starnix".as_ptr(),
        applicationVersion: 0,
        pEngineName: std::ptr::null(),
        engineVersion: 0,
        apiVersion: vulkan_version!(1, 1, 0),
    }
}

/// Returns the Vukan instance info for starnix.
pub fn instance_info(app_info: &vk::ApplicationInfo) -> vk::InstanceCreateInfo {
    vk::InstanceCreateInfo {
        sType: vk::STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        pNext: std::ptr::null(),
        flags: 0,
        pApplicationInfo: app_info,
        enabledLayerCount: 0,
        ppEnabledLayerNames: std::ptr::null(),
        enabledExtensionCount: 0,
        ppEnabledExtensionNames: std::ptr::null(),
    }
}

/// Registers a buffer collection with scenic and returns the associated import token.
///
/// # Parameters
/// - `buffer_collection_token`: The buffer collection token that is passed to the scenic allocator.
/// - `scenic_allocator`: The allocator proxy that is used to register the buffer collection.
fn register_buffer_collection_with_scenic(
    buffer_collection_token: ClientEnd<fsysmem2::BufferCollectionTokenMarker>,
    scenic_allocator: &fuicomp::AllocatorSynchronousProxy,
) -> Result<fuicomp::BufferCollectionImportToken, vk::Result> {
    let (scenic_import_token, export_token) = zx::EventPair::create();

    let export_token = fuicomp::BufferCollectionExportToken { value: export_token };
    let scenic_import_token = fuicomp::BufferCollectionImportToken { value: scenic_import_token };

    let args = fuicomp::RegisterBufferCollectionArgs {
        export_token: Some(export_token),
        // Sysmem token channels serve both sysmem(1) and sysmem2 token protocols, so we can convert
        // here until this protocol accepts a sysmem2 token.
        buffer_collection_token: Some(buffer_collection_token.into_channel().into()),
        ..Default::default()
    };

    scenic_allocator
        .register_buffer_collection(args, zx::Time::INFINITE)
        .map_err(|_| vk::ERROR_INITIALIZATION_FAILED)?
        .map_err(|_| vk::ERROR_INITIALIZATION_FAILED)?;

    Ok(scenic_import_token)
}
