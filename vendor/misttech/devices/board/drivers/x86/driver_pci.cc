// Copyright 2025 Mist Tecnologia Ltda
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

// Based on zircon/kernel/lib/syscalls/driver_pci.cc

#include "driver_pci.h"

#include <align.h>
#include <lib/pci/kpci.h>
#include <lib/pci/pio.h>
#include <platform.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/hw/pci.h>
#include <zircon/syscalls/pci.h>
#include <zircon/types.h>

#include <dev/address_provider/ecam_region.h>
#include <dev/interrupt.h>
#include <dev/pcie_bus_driver.h>
#include <dev/pcie_root.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <ktl/iterator.h>
#include <ktl/limits.h>
#include <ktl/unique_ptr.h>
#include <vm/vm_object_physical.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

// Implementation of a PcieRoot with a look-up table based legacy IRQ swizzler
// suitable for use with ACPI style swizzle definitions.
class PcieRootLUTSwizzle : public PcieRoot {
 public:
  static fbl::RefPtr<PcieRoot> Create(PcieBusDriver& bus_drv, uint managed_bus_id,
                                      const zx_pci_irq_swizzle_lut_t& lut) {
    fbl::AllocChecker ac;
    auto root = fbl::AdoptRef(new (&ac) PcieRootLUTSwizzle(bus_drv, managed_bus_id, lut));
    if (!ac.check()) {
      TRACEF("Out of memory attemping to create PCIe root to manage bus ID 0x%02x\n",
             managed_bus_id);
      return nullptr;
    }

    return root;
  }

  zx_status_t Swizzle(uint dev_id, uint func_id, uint pin, uint* irq) override {
    if ((irq == nullptr) || (dev_id >= ktl::size(lut_)) || (func_id >= ktl::size(lut_[dev_id])) ||
        (pin >= ktl::size(lut_[dev_id][func_id])))
      return ZX_ERR_INVALID_ARGS;

    *irq = lut_[dev_id][func_id][pin];
    return (*irq == ZX_PCI_NO_IRQ_MAPPING) ? ZX_ERR_NOT_FOUND : ZX_OK;
  }

 private:
  PcieRootLUTSwizzle(PcieBusDriver& bus_drv, uint managed_bus_id,
                     const zx_pci_irq_swizzle_lut_t& lut)
      : PcieRoot(bus_drv, managed_bus_id) {
    ::memcpy(&lut_, &lut, sizeof(lut_));
  }

  zx_pci_irq_swizzle_lut_t lut_;
};

// Scan |lut| for entries mapping to |irq|, and replace them with
// ZX_PCI_NO_IRQ_MAPPING.
static void pci_irq_swizzle_lut_remove_irq(zx_pci_irq_swizzle_lut_t* lut, uint32_t irq) {
  for (size_t dev = 0; dev < ktl::size(*lut); ++dev) {
    for (size_t func = 0; func < ktl::size((*lut)[dev]); ++func) {
      for (size_t pin = 0; pin < ktl::size((*lut)[dev][func]); ++pin) {
        uint32_t* assigned_irq = &(*lut)[dev][func][pin];
        if (*assigned_irq == irq) {
          *assigned_irq = ZX_PCI_NO_IRQ_MAPPING;
        }
      }
    }
  }
}

zx_status_t zx_pci_add_subtract_io_range(uint32_t mmio, uint64_t base, uint64_t len, uint32_t add) {
  if (!Pci::KernelPciEnabled()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bool is_add = (add > 0);
  bool is_mmio = (mmio > 0);
  LTRACEF("mmio %d base %#" PRIx64 " len %#" PRIx64 " add %d\n", is_mmio, base, len, is_add);

  auto pcie = PcieBusDriver::GetDriver();
  if (pcie == nullptr) {
    return ZX_ERR_BAD_STATE;
  }

  PciAddrSpace addr_space = is_mmio ? PciAddrSpace::MMIO : PciAddrSpace::PIO;

  if (is_add) {
    return pcie->AddBusRegion(base, len, addr_space);
  } else {
    return pcie->SubtractBusRegion(base, len, addr_space);
  }
}

static inline PciEcamRegion addr_window_to_pci_ecam_region(zx_pci_init_arg_t* arg, size_t index) {
  ASSERT(index < arg->addr_window_count);

  const PciEcamRegion result = {
      .phys_base = static_cast<paddr_t>(arg->addr_windows[index].base),
      .size = arg->addr_windows[index].size,
      .bus_start = arg->addr_windows[index].bus_start,
      .bus_end = arg->addr_windows[index].bus_end,
  };

  return result;
}

static inline bool is_designware(const zx_pci_init_arg_t* arg) {
  for (size_t i = 0; i < arg->addr_window_count; i++) {
    if ((arg->addr_windows[i].cfg_space_type == PCI_CFG_SPACE_TYPE_DW_ROOT) ||
        (arg->addr_windows[i].cfg_space_type == PCI_CFG_SPACE_TYPE_DW_DS)) {
      return true;
    }
  }

  return false;
}

zx_status_t zx_pci_init(zx_pci_init_arg_t* arg, uint32_t len) {
  if (!Pci::KernelPciEnabled()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto pcie = PcieBusDriver::GetDriver();
  if (pcie == nullptr)
    return ZX_ERR_BAD_STATE;

  const uint32_t win_count = arg->addr_window_count;

  if (arg->num_irqs > ktl::size(arg->irqs)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (LOCAL_TRACE) {
    const char* kAddrWindowTypes[] = {"PIO", "MMIO", "DW Root Bridge (MMIO)",
                                      "DW Downstream (MMIO)", "Unknown"};
    TRACEF("%u address window%s found in init arg\n", arg->addr_window_count,
           (arg->addr_window_count == 1) ? "" : "s");
    for (uint32_t i = 0; i < arg->addr_window_count; i++) {
      const size_t window_type_idx =
          ktl::min(ktl::size(kAddrWindowTypes) - 1,
                   static_cast<size_t>(arg->addr_windows[i].cfg_space_type));
      const char* window_type_name = kAddrWindowTypes[window_type_idx];

      printf("[%u]\n\tcfg_space_type: %s\n\thas_ecam: %d\n\tbase: %#" PRIxPTR
             "\n"
             "\tsize: %zu\n\tbus_start: %u\n\tbus_end: %u\n",
             i, window_type_name, arg->addr_windows[i].has_ecam, arg->addr_windows[i].base,
             arg->addr_windows[i].size, arg->addr_windows[i].bus_start,
             arg->addr_windows[i].bus_end);
    }
  }

  // Configure interrupts
  for (unsigned int i = 0; i < arg->num_irqs; ++i) {
    uint32_t irq = arg->irqs[i].global_irq;
    if (!is_valid_interrupt(irq, 0)) {
      // If the interrupt isn't valid, mask it out of the IRQ swizzle table
      // and don't activate it.  Attempts to use legacy IRQs for the device
      // will fail later.
      arg->irqs[i].global_irq = ZX_PCI_NO_IRQ_MAPPING;
      pci_irq_swizzle_lut_remove_irq(&arg->dev_pin_to_global_irq, irq);
      continue;
    }

    enum interrupt_trigger_mode tm = IRQ_TRIGGER_MODE_EDGE;
    if (arg->irqs[i].level_triggered) {
      tm = IRQ_TRIGGER_MODE_LEVEL;
    }
    enum interrupt_polarity pol = IRQ_POLARITY_ACTIVE_LOW;
    if (arg->irqs[i].active_high) {
      pol = IRQ_POLARITY_ACTIVE_HIGH;
    }

    zx_status_t configure_status = configure_interrupt(irq, tm, pol);
    if (configure_status != ZX_OK) {
      return configure_status;
    }
  }
  // TODO(teisenbe): For now assume there is only one ECAM, unless it's a
  // DesignWare Controller.
  // The DesignWare controller needs exactly two windows: One specifying where
  // the root bridge is and the other specifying where the downstream devices
  // are.
  if (is_designware(arg)) {
    if (win_count != 2) {
      return ZX_ERR_INVALID_ARGS;
    }
  } else {
    if (win_count != 1) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  if (arg->addr_windows[0].bus_start != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (arg->addr_windows[0].bus_start > arg->addr_windows[0].bus_end) {
    return ZX_ERR_INVALID_ARGS;
  }

#if ARCH_X86
  // Check for a quirk that we've seen.  Some systems will report overly large
  // PCIe config regions that collide with architectural registers.
  unsigned int num_buses = arg->addr_windows[0].bus_end - arg->addr_windows[0].bus_start + 1;
  paddr_t end = arg->addr_windows[0].base + num_buses * PCIE_ECAM_BYTE_PER_BUS;
  const paddr_t high_limit = 0xfec00000ULL;
  if (end > high_limit) {
    TRACEF("PCIe config space collides with arch devices, truncating\n");
    end = high_limit;
    if (end < arg->addr_windows[0].base) {
      return ZX_ERR_INVALID_ARGS;
    }
    arg->addr_windows[0].size = ROUNDDOWN(end - arg->addr_windows[0].base, PCIE_ECAM_BYTE_PER_BUS);
    uint64_t new_bus_end =
        (arg->addr_windows[0].size / PCIE_ECAM_BYTE_PER_BUS) + arg->addr_windows[0].bus_start - 1;
    if (new_bus_end >= PCIE_MAX_BUSSES) {
      return ZX_ERR_INVALID_ARGS;
    }
    arg->addr_windows[0].bus_end = static_cast<uint8_t>(new_bus_end);
  }
#endif

  if (arg->addr_windows[0].cfg_space_type == PCI_CFG_SPACE_TYPE_MMIO) {
    if (arg->addr_windows[0].size < PCIE_ECAM_BYTE_PER_BUS) {
      return ZX_ERR_INVALID_ARGS;
    }
    if (arg->addr_windows[0].size / PCIE_ECAM_BYTE_PER_BUS >
        PCIE_MAX_BUSSES - arg->addr_windows[0].bus_start) {
      return ZX_ERR_INVALID_ARGS;
    }

    // TODO(johngro): Update the syscall to pass a paddr_t for base instead of a uint64_t
    ASSERT(arg->addr_windows[0].base < ktl::numeric_limits<paddr_t>::max());

    fbl::AllocChecker ac;
    auto addr_provider = ktl::make_unique<MmioPcieAddressProvider>(&ac);
    if (!ac.check()) {
      TRACEF("Failed to allocate PCIe Address Provider\n");
      return ZX_ERR_NO_MEMORY;
    }

    // TODO(johngro): Do not limit this to a single range.  Instead, fetch all
    // of the ECAM ranges from ACPI, as well as the appropriate bus start/end
    // ranges.
    const PciEcamRegion ecam = {
        .phys_base = static_cast<paddr_t>(arg->addr_windows[0].base),
        .size = arg->addr_windows[0].size,
        .bus_start = 0x00,
        .bus_end = static_cast<uint8_t>((arg->addr_windows[0].size / PCIE_ECAM_BYTE_PER_BUS) - 1),
    };

    zx_status_t ret = addr_provider->AddEcamRegion(ecam);
    if (ret != ZX_OK) {
      TRACEF("Failed to add ECAM region to PCIe bus driver! (ret %d)\n", ret);
      return ret;
    }

    ret = pcie->SetAddressTranslationProvider(ktl::move(addr_provider));
    if (ret != ZX_OK) {
      TRACEF("Failed to set Address Translation Provider, st = %d\n", ret);
      return ret;
    }
  } else if (arg->addr_windows[0].cfg_space_type == PCI_CFG_SPACE_TYPE_PIO) {
    // Create a PIO address provider.
    fbl::AllocChecker ac;

    auto addr_provider = ktl::make_unique<PioPcieAddressProvider>(&ac);
    if (!ac.check()) {
      TRACEF("Failed to allocate PCIe address provider\n");
      return ZX_ERR_NO_MEMORY;
    }

    zx_status_t ret = pcie->SetAddressTranslationProvider(ktl::move(addr_provider));
    if (ret != ZX_OK) {
      TRACEF("Failed to set Address Translation Provider, st = %d\n", ret);
      return ret;
    }
  } else if (is_designware(arg)) {
    fbl::AllocChecker ac;

    if (win_count < 2) {
      TRACEF("DesignWare Config Space requires at least 2 windows\n");
      return ZX_ERR_INVALID_ARGS;
    }

    auto addr_provider = ktl::make_unique<DesignWarePcieAddressProvider>(&ac);
    if (!ac.check()) {
      TRACEF("Failed to allocate PCIe address provider\n");
      return ZX_ERR_NO_MEMORY;
    }

    PciEcamRegion dw_root_bridge{};
    PciEcamRegion dw_downstream{};
    for (size_t i = 0; i < win_count; i++) {
      switch (arg->addr_windows[i].cfg_space_type) {
        case PCI_CFG_SPACE_TYPE_DW_ROOT:
          dw_root_bridge = addr_window_to_pci_ecam_region(arg, i);
          break;
        case PCI_CFG_SPACE_TYPE_DW_DS:
          dw_downstream = addr_window_to_pci_ecam_region(arg, i);
          break;
      }
    }

    if (dw_root_bridge.size == 0 || dw_downstream.size == 0) {
      TRACEF("Did not find DesignWare root and downstream device in init arg\n");
      return ZX_ERR_INVALID_ARGS;
    }

    zx_status_t ret = addr_provider->Init(dw_root_bridge, dw_downstream);
    if (ret != ZX_OK) {
      TRACEF("Failed to initialize DesignWare PCIe Address Provider, error = %d\n", ret);
      return ret;
    }

    ret = pcie->SetAddressTranslationProvider(ktl::move(addr_provider));
    if (ret != ZX_OK) {
      TRACEF("Failed to set Address Translation Provider, st = %d\n", ret);
      return ret;
    }

  } else {
    TRACEF("Unknown config space type!\n");
    return ZX_ERR_INVALID_ARGS;
  }
  // TODO(johngro): Change the user-mode and devmgr behavior to add all of the
  // roots in the system.  Do not assume that there is a single root, nor that
  // it manages bus ID 0.
  auto root = PcieRootLUTSwizzle::Create(*pcie, 0, arg->dev_pin_to_global_irq);
  if (root == nullptr)
    return ZX_ERR_NO_MEMORY;

  zx_status_t ret = pcie->AddRoot(ktl::move(root));
  if (ret != ZX_OK) {
    TRACEF("Failed to add root complex to PCIe bus driver! (ret %d)\n", ret);
    return ret;
  }

  ret = pcie->StartBusDriver();
  if (ret != ZX_OK) {
    TRACEF("Failed to start PCIe bus driver! (ret %d)\n", ret);
    return ret;
  }

  return ZX_OK;
}
