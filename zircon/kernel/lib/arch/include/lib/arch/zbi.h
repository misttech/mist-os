// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_ZBI_H_
#define ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_ZBI_H_

#include <lib/zbi-format/kernel.h>
#include <lib/zbi-format/zbi.h>

namespace arch {

// Represents (the headers of) a bootable ZBI, loaded into memory by the boot
// loader.
struct ZbiKernelImage {
  zbi_header_t hdr_file;
  zbi_header_t hdr_kernel;
  zbi_kernel_t data_kernel;
};

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_ZBI_H_
