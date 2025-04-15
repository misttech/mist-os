// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BUS_DRIVERS_PCI_COMMON_H_
#define SRC_DEVICES_BUS_DRIVERS_PCI_COMMON_H_

#include <lib/ddk/debug.h>
#include <stdarg.h>
#include <stdio.h>
#include <zircon/hw/pci.h>

#include <array>

/*
 * PCI access return codes
 */
#define _PCI_SUCCESSFUL 0x00
#define _PCI_FUNC_NOT_SUPPORTED 0x81
#define _PCI_BAD_VENDOR_ID 0x83
#define _PCI_DEVICE_NOT_FOUND 0x86
#define _PCI_BAD_REGISTER_NUMBER 0x87
#define _PCI_SET_FAILED 0x88
#define _PCI_BUFFER_TOO_SMALL 0x89

#define PCI_CONFIG_HDR_SIZE (64u)
#define PCI_BASE_CONFIG_SIZE (256u)
#define PCI_EXT_CONFIG_SIZE (4096u)

/*
 * PCI command register bits
 */

// clang-format off
#define PCI_CONFIG_COMMAND_IO_EN         (1u << 0)
#define PCI_CONFIG_COMMAND_MEM_EN        (1u << 1)
#define PCI_CONFIG_COMMAND_BUS_MASTER_EN (1u << 2)
#define PCI_CONFIG_COMMAND_SPECIAL_EN    (1u << 3)
#define PCI_CONFIG_COMMAND_MEM_WR_INV_EN (1u << 4)
#define PCI_CONFIG_COMMAND_PAL_SNOOP_EN  (1u << 5)
#define PCI_CONFIG_COMMAND_PERR_RESP_EN  (1u << 6)
#define PCI_CONFIG_COMMAND_AD_STEP_EN    (1u << 7)
#define PCI_CONFIG_COMMAND_SERR_EN       (1u << 8)
#define PCI_CONFIG_COMMAND_FAST_B2B_EN   (1u << 9)
#define PCI_CONFIG_COMMAND_INT_DISABLE   (1u << 10)
// clang-format on

/**
 * The maximum possible number of standard capabilities for a PCI
 * device/function is 48.  This comes from the facts that...
 *
 * ++ There are 256 bytes in the standard configuration space.
 * ++ The first 64 bytes are used by the standard configuration header, leaving
 *    192 bytes for capabilities.
 * ++ Even though the capability header is only 2 bytes long, it must be aligned
 *    on a 4 byte boundary.  The means that one can pack (at most) 192 / 4 == 48
 *    properly aligned standard PCI capabilities.
 *
 * Similar logic may be applied to extended capabilities which must also be 4
 * byte aligned, but exist in the region after the standard configuration block.
 */
#define PCI_CAPABILITY_ALIGNMENT (4u)

#define PCI_MAX_CAPABILITIES \
  ((PCI_BASE_CONFIG_SIZE - PCI_STANDARD_CONFIG_HDR_SIZE) / PCI_CAPABILITY_ALIGNMENT)
#define PCI_CAP_PTR_NULL (0u)
#define PCI_CAP_PTR_MIN_VALID (PCI_STANDARD_CONFIG_HDR_SIZE)
#define PCI_CAP_PTR_MAX_VALID (PCI_BASE_CONFIG_SIZE - PCI_CAPABILITY_ALIGNMENT)
#define PCI_CAP_PTR_ALIGNMENT (2u)

#define PCIE_EXT_CAP_PTR_NULL (0u)
#define PCIE_EXT_CAP_BASE_PTR (0x100)
#define PCIE_EXT_CAP_PTR_MIN_VALID (PCI_BASE_CONFIG_SIZE)
#define PCIE_EXT_CAP_PTR_MAX_VALID (PCIE_EXTENDED_CONFIG_SIZE - PCI_CAPABILITY_ALIGNMENT)
#define PCIE_EXT_CAP_PTR_ALIGNMENT (4u)
#define PCIE_MAX_EXT_CAPABILITIES \
  ((PCIE_EXTENDED_CONFIG_SIZE - PCI_BASE_CONFIG_SIZE) / PCI_CAPABILITY_ALIGNMENT)

/*
 * PCI BAR register masks and constants
 */
#define PCI_BAR_IO_TYPE_MASK (0x00000001)
#define PCI_BAR_IO_TYPE_MMIO (0x00000000)
#define PCI_BAR_IO_TYPE_PIO (0x00000001)

#define PCI_BAR_MMIO_TYPE_MASK (0x00000006)
#define PCI_BAR_MMIO_TYPE_32BIT (0x00000000)
#define PCI_BAR_MMIO_TYPE_64BIT (0x00000004)

#define PCI_BAR_MMIO_PREFETCH_MASK (0x00000008)
#define PCI_BAR_MMIO_ADDR_MASK (0xFFFFFFF0)
#define PCI_BAR_PIO_ADDR_MASK (0xFFFFFFFC)

#define PCI_LEGACY_INT_COUNT 1
/*
 * Extra bits used in the CFG command and status registers defined by PCIe.  See
 * the PCIe Base Specification, sections 7.5.1.1 and 7.5.1.2
 */
#define PCIE_CFG_COMMAND_INT_DISABLE ((uint16_t)(1 << 10))
#define PCIE_CFG_STATUS_INT_STS ((uint16_t)(1 << 3))

#ifdef __x86_64__
constexpr bool PCI_HAS_IO_ADDR_SPACE = true;
constexpr uint64_t PCI_PIO_ADDR_SPACE_MASK = 0xFFFF;
constexpr uint64_t PCI_PIO_ADDR_SPACE_SIZE = 0x10000;
#else   // non-x86
constexpr bool PCI_HAS_IO_ADDR_SPACE = false;
constexpr uint64_t PCI_PIO_ADDR_SPACE_MASK = 0xFFFFFFFF;
constexpr uint64_t PCI_PIO_ADDR_SPACE_SIZE = 0x100000000;
#endif  // #if defined(__x86_64__)

#endif  // SRC_DEVICES_BUS_DRIVERS_PCI_COMMON_H_
