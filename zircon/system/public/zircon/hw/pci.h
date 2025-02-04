// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_HW_PCI_H_
#define ZIRCON_HW_PCI_H_

#include <stdint.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

// Structure for passing around PCI address information
typedef struct zx_pci_bdf {
  uint8_t bus_id;
  uint8_t device_id;
  uint8_t function_id;
} zx_pci_bdf_t;

#define PCI_MAX_BUSES (256u)
#define PCI_MAX_DEVICES_PER_BUS (32u)
#define PCI_MAX_FUNCTIONS_PER_DEVICE (8u)
#define PCI_MAX_FUNCTIONS_PER_BUS (PCI_MAX_DEVICES_PER_BUS * PCI_MAX_FUNCTIONS_PER_DEVICE)

#define PCI_STANDARD_CONFIG_HDR_SIZE (64u)
#define PCI_BASE_CONFIG_SIZE (256u)
#define PCIE_EXTENDED_CONFIG_SIZE (4096u)
#define PCIE_ECAM_BYTES_PER_BUS (PCIE_EXTENDED_CONFIG_SIZE * PCI_MAX_FUNCTIONS_PER_BUS)

#define PCI_BAR_REGS_PER_BRIDGE (2u)
#define PCI_BAR_REGS_PER_DEVICE (6u)
#define PCI_MAX_BAR_REGS (6u)

#define PCI_MAX_LEGACY_IRQ_PINS (4u)
#define PCI_MAX_MSI_IRQS (32u)
#define PCIE_MAX_MSIX_IRQS (2048u)

#define PCI_INVALID_VENDOR_ID (0xFFFF)

__END_CDECLS

#endif  // ZIRCON_HW_PCI_H_
