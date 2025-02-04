// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_KPCI_INCLUDE_LIB_PCI_KPCI_H_
#define ZIRCON_KERNEL_LIB_KPCI_INCLUDE_LIB_PCI_KPCI_H_

#include <lib/boot-options/boot-options.h>

namespace Pci {

static inline bool KernelPciEnabled() {
#if defined(__aarch64__)
  return gBootOptions->arm64_kernel_pci;
#else
  // Kernel PCI support is disabled on non-aarch64 platforms.
  return false;
#endif
}

}  // namespace Pci

#endif  // ZIRCON_KERNEL_LIB_KPCI_INCLUDE_LIB_PCI_KPCI_H_
