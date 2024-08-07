// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Generated by ./bindgen.sh using bindgen 0.66.1

// Allow non-conventional naming for imports from C/C++.
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::undocumented_unsafe_blocks)]

use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

// Configure linkage for MacOS.
#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct zbi_kernel_t {
    pub entry: u64,
    pub reserve_memory_size: u64,
}
#[test]
fn bindgen_test_layout_zbi_kernel_t() {
    const UNINIT: ::std::mem::MaybeUninit<zbi_kernel_t> = ::std::mem::MaybeUninit::uninit();
    let ptr = UNINIT.as_ptr();
    assert_eq!(
        ::std::mem::size_of::<zbi_kernel_t>(),
        16usize,
        concat!("Size of: ", stringify!(zbi_kernel_t))
    );
    assert_eq!(
        ::std::mem::align_of::<zbi_kernel_t>(),
        8usize,
        concat!("Alignment of ", stringify!(zbi_kernel_t))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).entry) as usize - ptr as usize },
        0usize,
        concat!("Offset of field: ", stringify!(zbi_kernel_t), "::", stringify!(entry))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).reserve_memory_size) as usize - ptr as usize },
        8usize,
        concat!(
            "Offset of field: ",
            stringify!(zbi_kernel_t),
            "::",
            stringify!(reserve_memory_size)
        )
    );
}
pub const ZBI_ALIGNMENT: u32 = 8;
pub const ZBI_TYPE_KERNEL_PREFIX: u32 = 5132875;
pub const ZBI_TYPE_KERNEL_MASK: u32 = 16777215;
pub const ZBI_TYPE_DRIVER_METADATA_PREFIX: u32 = 109;
pub const ZBI_TYPE_DRIVER_METADATA_MASK: u32 = 255;
pub type zbi_type_t = u32;
pub const ZBI_TYPE_CONTAINER: zbi_type_t = 1414483778;
pub const ZBI_TYPE_KERNEL_X64: zbi_type_t = 1280201291;
pub const ZBI_TYPE_KERNEL_ARM64: zbi_type_t = 944656971;
pub const ZBI_TYPE_KERNEL_RISCV64: zbi_type_t = 1447973451;
pub const ZBI_TYPE_DISCARD: zbi_type_t = 1346980691;
pub const ZBI_TYPE_STORAGE_RAMDISK: zbi_type_t = 1263748178;
pub const ZBI_TYPE_STORAGE_BOOTFS: zbi_type_t = 1112753730;
pub const ZBI_TYPE_STORAGE_KERNEL: zbi_type_t = 1381258059;
pub const ZBI_TYPE_STORAGE_BOOTFS_FACTORY: zbi_type_t = 1179862594;
pub const ZBI_TYPE_CMDLINE: zbi_type_t = 1279544643;
pub const ZBI_TYPE_CRASHLOG: zbi_type_t = 1297043266;
pub const ZBI_TYPE_NVRAM: zbi_type_t = 1280071246;
pub const ZBI_TYPE_PLATFORM_ID: zbi_type_t = 1145654352;
pub const ZBI_TYPE_DRV_BOARD_INFO: zbi_type_t = 1230193261;
pub const ZBI_TYPE_CPU_TOPOLOGY: zbi_type_t = 1129338163;
pub const ZBI_TYPE_MEM_CONFIG: zbi_type_t = 1129137485;
pub const ZBI_TYPE_KERNEL_DRIVER: zbi_type_t = 1448232011;
pub const ZBI_TYPE_ACPI_RSDP: zbi_type_t = 1346655058;
pub const ZBI_TYPE_SMBIOS: zbi_type_t = 1229081939;
pub const ZBI_TYPE_EFI_SYSTEM_TABLE: zbi_type_t = 1397311045;
pub const ZBI_TYPE_EFI_MEMORY_ATTRIBUTES_TABLE: zbi_type_t = 1413565765;
pub const ZBI_TYPE_FRAMEBUFFER: zbi_type_t = 1111906131;
pub const ZBI_TYPE_IMAGE_ARGS: zbi_type_t = 1196573001;
pub const ZBI_TYPE_BOOT_VERSION: zbi_type_t = 1397904962;
pub const ZBI_TYPE_DRV_MAC_ADDRESS: zbi_type_t = 1128353133;
pub const ZBI_TYPE_DRV_PARTITION_MAP: zbi_type_t = 1414680685;
pub const ZBI_TYPE_DRV_BOARD_PRIVATE: zbi_type_t = 1380926061;
pub const ZBI_TYPE_HW_REBOOT_REASON: zbi_type_t = 1112692552;
pub const ZBI_TYPE_SERIAL_NUMBER: zbi_type_t = 1313624659;
pub const ZBI_TYPE_BOOTLOADER_FILE: zbi_type_t = 1279677506;
pub const ZBI_TYPE_DEVICETREE: zbi_type_t = 3490578157;
pub const ZBI_TYPE_SECURE_ENTROPY: zbi_type_t = 1145979218;
pub const ZBI_TYPE_DEBUGDATA: zbi_type_t = 1145520708;
pub const ZBI_CONTAINER_MAGIC: u32 = 2257385446;
pub const ZBI_ITEM_MAGIC: u32 = 3044546345;
pub type zbi_flags_t = u32;
pub const ZBI_FLAGS_VERSION: zbi_flags_t = 65536;
pub const ZBI_FLAGS_CRC32: zbi_flags_t = 131072;
pub const ZBI_ITEM_NO_CRC32: u32 = 1250420950;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq, FromBytes, AsBytes, FromZeros, NoCell)]
pub struct zbi_header_t {
    pub type_: zbi_type_t,
    pub length: u32,
    pub extra: u32,
    pub flags: zbi_flags_t,
    pub reserved0: u32,
    pub reserved1: u32,
    pub magic: u32,
    pub crc32: u32,
}
#[test]
fn bindgen_test_layout_zbi_header_t() {
    const UNINIT: ::std::mem::MaybeUninit<zbi_header_t> = ::std::mem::MaybeUninit::uninit();
    let ptr = UNINIT.as_ptr();
    assert_eq!(
        ::std::mem::size_of::<zbi_header_t>(),
        32usize,
        concat!("Size of: ", stringify!(zbi_header_t))
    );
    assert_eq!(
        ::std::mem::align_of::<zbi_header_t>(),
        4usize,
        concat!("Alignment of ", stringify!(zbi_header_t))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).type_) as usize - ptr as usize },
        0usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(type_))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).length) as usize - ptr as usize },
        4usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(length))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).extra) as usize - ptr as usize },
        8usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(extra))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).flags) as usize - ptr as usize },
        12usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(flags))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).reserved0) as usize - ptr as usize },
        16usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(reserved0))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).reserved1) as usize - ptr as usize },
        20usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(reserved1))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).magic) as usize - ptr as usize },
        24usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(magic))
    );
    assert_eq!(
        unsafe { ::std::ptr::addr_of!((*ptr).crc32) as usize - ptr as usize },
        28usize,
        concat!("Offset of field: ", stringify!(zbi_header_t), "::", stringify!(crc32))
    );
}
