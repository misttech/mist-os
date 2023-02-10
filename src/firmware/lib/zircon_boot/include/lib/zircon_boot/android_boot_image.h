// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Definitions of the Android boot image layout, to support booting from RAM via
// `fastboot boot` which automatically wraps the provided image in this format.
//
// Upstream definitions are at
// https://android.googlesource.com/platform/system/tools/mkbootimg/+/refs/heads/master/include/bootimg/bootimg.h,
// modified here for C compatibility and renaming to avoid collisions.

#ifndef SRC_FIRMWARE_LIB_ZIRCON_BOOT_INCLUDE_LIB_ZIRCON_BOOT_ANDROID_BOOT_IMAGE_H_
#define SRC_FIRMWARE_LIB_ZIRCON_BOOT_INCLUDE_LIB_ZIRCON_BOOT_ANDROID_BOOT_IMAGE_H_

#ifdef ZIRCON_BOOT_CUSTOM_SYSDEPS_HEADER
#include <zircon_boot_sysdeps.h>
#else
#include <stdbool.h>
#include <stdint.h>
#endif

#define ZIRCON_BOOT_ANDROID_IMAGE_MAGIC "ANDROID!"
#define ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE 8
#define ZIRCON_BOOT_ANDROID_IMAGE_NAME_SIZE 16
#define ZIRCON_BOOT_ANDROID_IMAGE_ARGS_SIZE 512
#define ZIRCON_BOOT_ANDROID_IMAGE_EXTRA_ARGS_SIZE 1024

typedef struct {
  uint8_t magic[ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE];
  uint32_t kernel_size;
  uint32_t kernel_addr;
  uint32_t ramdisk_size;
  uint32_t ramdisk_addr;
  uint32_t second_size;
  uint32_t second_addr;
  uint32_t tags_addr;
  uint32_t page_size;
  uint32_t header_version;
  uint32_t os_version;
  uint8_t name[ZIRCON_BOOT_ANDROID_IMAGE_NAME_SIZE];
  uint8_t cmdline[ZIRCON_BOOT_ANDROID_IMAGE_ARGS_SIZE];
  uint32_t id[8];
  uint8_t extra_cmdline[ZIRCON_BOOT_ANDROID_IMAGE_EXTRA_ARGS_SIZE];
} __attribute((packed)) zircon_boot_android_image_hdr_v0;

typedef struct {
  uint8_t magic[ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE];
  uint32_t kernel_size;
  uint32_t kernel_addr;
  uint32_t ramdisk_size;
  uint32_t ramdisk_addr;
  uint32_t second_size;
  uint32_t second_addr;
  uint32_t tags_addr;
  uint32_t page_size;
  uint32_t header_version;
  uint32_t os_version;
  uint8_t name[ZIRCON_BOOT_ANDROID_IMAGE_NAME_SIZE];
  uint8_t cmdline[ZIRCON_BOOT_ANDROID_IMAGE_ARGS_SIZE];
  uint32_t id[8];
  uint8_t extra_cmdline[ZIRCON_BOOT_ANDROID_IMAGE_EXTRA_ARGS_SIZE];

  uint32_t recovery_dtbo_size;
  uint64_t recovery_dtbo_offset;
  uint32_t header_size;
} __attribute((packed)) zircon_boot_android_image_hdr_v1;

typedef struct {
  uint8_t magic[ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE];
  uint32_t kernel_size;
  uint32_t kernel_addr;
  uint32_t ramdisk_size;
  uint32_t ramdisk_addr;
  uint32_t second_size;
  uint32_t second_addr;
  uint32_t tags_addr;
  uint32_t page_size;
  uint32_t header_version;
  uint32_t os_version;
  uint8_t name[ZIRCON_BOOT_ANDROID_IMAGE_NAME_SIZE];
  uint8_t cmdline[ZIRCON_BOOT_ANDROID_IMAGE_ARGS_SIZE];
  uint32_t id[8];
  uint8_t extra_cmdline[ZIRCON_BOOT_ANDROID_IMAGE_EXTRA_ARGS_SIZE];

  uint32_t recovery_dtbo_size;
  uint64_t recovery_dtbo_offset;
  uint32_t header_size;

  uint32_t dtb_size;
  uint64_t dtb_addr;
} __attribute((packed)) zircon_boot_android_image_hdr_v2;

typedef struct {
  uint8_t magic[ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE];
  uint32_t kernel_size;
  uint32_t ramdisk_size;
  uint32_t os_version;
  uint32_t header_size;
  uint32_t reserved[4];
  uint32_t header_version;
  uint8_t cmdline[ZIRCON_BOOT_ANDROID_IMAGE_ARGS_SIZE + ZIRCON_BOOT_ANDROID_IMAGE_EXTRA_ARGS_SIZE];
} __attribute((packed)) zircon_boot_android_image_hdr_v3;

typedef struct {
  uint8_t magic[ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE];
  uint32_t kernel_size;
  uint32_t ramdisk_size;
  uint32_t os_version;
  uint32_t header_size;
  uint32_t reserved[4];
  uint32_t header_version;
  uint8_t cmdline[ZIRCON_BOOT_ANDROID_IMAGE_ARGS_SIZE + ZIRCON_BOOT_ANDROID_IMAGE_EXTRA_ARGS_SIZE];

  uint32_t signature_size;
} __attribute((packed)) zircon_boot_android_image_hdr_v4;

// Locates the kernel in an Android boot image.
//
// Note that for Fuchsia, the "kernel" contained in the Android image may
// be just the ZBI, or a combined ZBI+vbmeta.
//
// @image: pointer to the Android boot image. Must be properly-aligned.
// @size: Android boot image size (header + contents).
// @kernel: on success will point to the kernel image.
// @kernel_size: on success will be filled with the kernel size.
//
// Returns true on success.
bool zircon_boot_get_android_image_kernel(const void* image, size_t size, const void** kernel,
                                          size_t* kernel_size);

#endif  // SRC_FIRMWARE_LIB_ZIRCON_BOOT_INCLUDE_LIB_ZIRCON_BOOT_ANDROID_BOOT_IMAGE_H_
