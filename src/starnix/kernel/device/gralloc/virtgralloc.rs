// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zerocopy::{AsBytes, FromBytes, FromZeroes, NoCell};

pub type __u32 = ::std::os::raw::c_uint;
pub type __u64 = ::std::os::raw::c_ulonglong;

pub const virtgralloc_request_SetVulkanMode: __u32 = 0xC0106701;

pub type virtgralloc_VulkanMode = __u64;
#[allow(unused)]
pub const virtgralloc_VulkanMode_INVALID: virtgralloc_VulkanMode = 0;
#[allow(unused)]
pub const virtgralloc_VulkanMode_SWIFTSHADER: virtgralloc_VulkanMode = 1;
#[allow(unused)]
pub const virtgralloc_VulkanMode_MAGMA: virtgralloc_VulkanMode = 2;

pub type virtgralloc_SetVulkanModeResult = __u64;
#[allow(unused)]
pub const virtgralloc_SetVulkanModeResult_INVALID: virtgralloc_SetVulkanModeResult = 0;
pub const virtgralloc_SetVulkanModeResult_SUCCESS: virtgralloc_SetVulkanModeResult = 1;

#[repr(C, packed)]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeroes, NoCell)]
pub struct virtgralloc_set_vulkan_mode {
    // Must be a virtgralloc_VulkanMode_* value other than INVALID.
    pub vulkan_mode: virtgralloc_VulkanMode,
    pub result: virtgralloc_SetVulkanModeResult,
}
