// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifdef ZIRCON_BOOT_CUSTOM_SYSDEPS_HEADER
#include <zircon_boot_sysdeps.h>
#else
#include <string.h>
#endif

#include <lib/zircon_boot/android_boot_image.h>

#include "utils.h"

bool zircon_boot_get_android_image_kernel(const void* image, size_t size, const void** kernel,
                                          size_t* kernel_size) {
  if (memcmp(image, ZIRCON_BOOT_ANDROID_IMAGE_MAGIC, ZIRCON_BOOT_ANDROID_IMAGE_MAGIC_SIZE)) {
    return false;
  }

  // Extract the kernel offset and size from the header. The kernel always
  // starts on the page following the header so the kernel offset will be
  // the page size.
  size_t offset = 0, header_size = 0;
  const zircon_boot_android_image_hdr_v0* v0_hdr = (const zircon_boot_android_image_hdr_v0*)image;
  switch (v0_hdr->header_version) {
    case 0:
      header_size = sizeof(zircon_boot_android_image_hdr_v0);
      offset = v0_hdr->page_size;
      *kernel_size = v0_hdr->kernel_size;
      break;
    case 1:
      header_size = sizeof(zircon_boot_android_image_hdr_v1);
      offset = ((zircon_boot_android_image_hdr_v1*)image)->page_size;
      *kernel_size = ((zircon_boot_android_image_hdr_v1*)image)->kernel_size;
      break;
    case 2:
      header_size = sizeof(zircon_boot_android_image_hdr_v2);
      offset = ((zircon_boot_android_image_hdr_v2*)image)->page_size;
      *kernel_size = ((zircon_boot_android_image_hdr_v2*)image)->kernel_size;
      break;
    case 3:
      header_size = sizeof(zircon_boot_android_image_hdr_v3);
      offset = 4096;  // Version 3 hardcodes the page size at 4096.
      *kernel_size = ((zircon_boot_android_image_hdr_v3*)image)->kernel_size;
      break;
    case 4:
      header_size = sizeof(zircon_boot_android_image_hdr_v4);
      offset = 4096;  // Version 4 hardcodes the page size at 4096.
      *kernel_size = ((zircon_boot_android_image_hdr_v4*)image)->kernel_size;
      break;
    default:
      zircon_boot_dlog("Unsupported Android boot image version (%u)\n", v0_hdr->header_version);
      return false;
  }

  // Finally check that the provided image is actually big enough to hold the
  // header and the kernel.
  if (header_size > size || offset + *kernel_size > size) {
    zircon_boot_dlog("Malformed Android boot image\n");
    return false;
  }

  *kernel = (const uint8_t*)image + offset;

  return true;
}
