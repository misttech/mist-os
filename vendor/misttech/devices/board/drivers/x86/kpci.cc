// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <stdio.h>
#include <trace.h>
#include <zircon/hw/pci.h>
#include <zircon/syscalls/pci.h>

#include <acpica/acpi.h>

#include "acpi-private.h"
#include "driver_pci.h"
#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/device-args.h"
#include "src/devices/board/lib/acpi/manager.h"
#include "src/devices/board/lib/acpi/resources.h"
#include "src/devices/board/lib/acpi/status.h"

#define PCI_HID ((char*)"PNP0A03")
#define PCIE_HID ((char*)"PNP0A08")
#define xprintf(fmt...) LTRACEF(fmt)

#define LOCAL_TRACE 0

/* Helper routine for translating IRQ routing tables into usable form
 *
 * @param port_dev_id The device ID on the root bus of this root port or
 * UINT8_MAX if this call is for the root bus, not a root port
 * @param port_func_id The function ID on the root bus of this root port or
 * UINT8_MAX if this call is for the root bus, not a root port
 */
static ACPI_STATUS handle_prt(ACPI_HANDLE object, zx_pci_init_arg_t* arg, uint8_t port_dev_id,
                              uint8_t port_func_id) {
  assert((port_dev_id == UINT8_MAX && port_func_id == UINT8_MAX) ||
         (port_dev_id != UINT8_MAX && port_func_id != UINT8_MAX));

  ACPI_BUFFER buffer = {
      // Request that the ACPI subsystem allocate the buffer
      .Length = ACPI_ALLOCATE_BUFFER,
      .Pointer = NULL,
  };
  ACPI_BUFFER crs_buffer = {
      .Length = ACPI_ALLOCATE_BUFFER,
      .Pointer = NULL,
  };

  ACPI_STATUS status = AcpiGetIrqRoutingTable(object, &buffer);
  // IRQ routing tables are *required* to exist on the root hub
  if (status != AE_OK) {
    goto cleanup;
  }

  uintptr_t entry_addr;
  entry_addr = reinterpret_cast<uintptr_t>(buffer.Pointer);
  ACPI_PCI_ROUTING_TABLE* entry;
  for (entry = (ACPI_PCI_ROUTING_TABLE*)entry_addr; entry->Length != 0;
       entry_addr += entry->Length, entry = (ACPI_PCI_ROUTING_TABLE*)entry_addr) {
    if (entry_addr > (uintptr_t)buffer.Pointer + buffer.Length) {
      return AE_ERROR;
    }
    if (entry->Pin >= PCI_MAX_LEGACY_IRQ_PINS) {
      printf("PRT entry contains an invalid pin: %#x\n", entry->Pin);
      return AE_ERROR;
    }

    LTRACEF(
        "_PRT Entry RootPort %02x.%1x: .Address = 0x%05llx, .Pin = %u, .SourceIndex = %u, "
        ".Source = \"%s\"\n",
        (port_dev_id != UINT8_MAX) ? port_dev_id : 0,
        (port_func_id != UINT8_MAX) ? port_func_id : 0, entry->Address, entry->Pin,
        entry->SourceIndex, entry->u.Source);

    // Per ACPI Spec 6.2.13, all _PRT entries must have a function address of
    // 0xFFFF representing all functions in the device. In effect, this means we
    // only care about the entry's dev id.
    uint8_t dev_id = (entry->Address >> 16) & (PCI_MAX_DEVICES_PER_BUS - 1);
    // Either we're handling the root complex (port_dev_id == UINT8_MAX), or
    // we're handling a root port, and if it's a root port, dev_id should
    // be 0. If not, the entry is strange and we'll warn / skip it.
    if (port_dev_id != UINT8_MAX && dev_id != 0) {
      // this is a weird entry, skip it
      LTRACEF("PRT entry for root unexpected contains device address: %#x\n", dev_id);
      continue;
    }

    // By default, SourceIndex refers to a global IRQ number that the pin is
    // connected to and we assume the legacy defaults of Level-triggered / Active Low.
    // PCI Local Bus Specification 3.0 section 2.2.6
    uint32_t global_irq = entry->SourceIndex;
    bool level_triggered = true;
    bool active_high = false;

#if 0
    // If the PRT contains a Source entry than we can attempt to find an Extended
    // IRQ Resource describing it.
    if (entry->u.Source[0]) {
      // If the Source is not just a NULL byte, then it refers to a
      // PCI Interrupt Link Device
      ACPI_HANDLE ild;
      status = AcpiGetHandle(object, entry->u.Source, &ild);
      if (status != AE_OK) {
        printf("Failed to get handle for PCI interrupt link device %s: %d\n", entry->u.Source,
               status);
      }
      status = AcpiGetCurrentResources(ild, &crs_buffer);
      if (status != AE_OK) {
        printf("Failed to get current resources for interrupt link device: %d\n", status);
      }

      uintptr_t crs_entry_addr = (uintptr_t)crs_buffer.Pointer;
      ACPI_RESOURCE* res = (ACPI_RESOURCE*)crs_entry_addr;
      while (res->Type != ACPI_RESOURCE_TYPE_END_TAG) {
        if (res->Type == ACPI_RESOURCE_TYPE_EXTENDED_IRQ) {
          ACPI_RESOURCE_EXTENDED_IRQ* irq = &res->Data.ExtendedIrq;
          if (global_irq != ZX_PCI_NO_IRQ_MAPPING) {
            // TODO: Handle finding two allocated IRQs.  Shouldn't
            // happen?
            PANIC_UNIMPLEMENTED;
          }
          if (irq->InterruptCount != 1) {
            // TODO: Handle finding two allocated IRQs.  Shouldn't
            // happen?
            PANIC_UNIMPLEMENTED;
          }
          if (irq->u.Interrupts[0] != 0) {
            active_high = (irq->Polarity == ACPI_ACTIVE_HIGH);
            level_triggered = (irq->Triggering == ACPI_LEVEL_SENSITIVE);
            global_irq = irq->u.Interrupts[0];
          }
        } else {
          // TODO: Handle non extended IRQs
          PANIC_UNIMPLEMENTED;
        }
        crs_entry_addr += res->Length;
        res = (ACPI_RESOURCE*)crs_entry_addr;
      }
      if (global_irq == ZX_PCI_NO_IRQ_MAPPING) {
        // TODO: Invoke PRS to find what is allocatable and allocate it with SRS
        PANIC_UNIMPLEMENTED;
      }
      AcpiOsFree(crs_buffer.Pointer);
      crs_buffer.Length = ACPI_ALLOCATE_BUFFER;
      crs_buffer.Pointer = NULL;
    }
#endif

    // Check if we've seen this IRQ already, and if so, confirm the
    // IRQ signaling is the same.
    bool found_irq = false;
    for (unsigned int i = 0; i < arg->num_irqs; ++i) {
      if (global_irq != arg->irqs[i].global_irq) {
        continue;
      }
      if (active_high != arg->irqs[i].active_high ||
          level_triggered != arg->irqs[i].level_triggered) {
        // TODO: Handle mismatch here
        PANIC_UNIMPLEMENTED;
      }
      found_irq = true;
      break;
    }
    if (!found_irq) {
      assert(arg->num_irqs < std::size(arg->irqs));
      arg->irqs[arg->num_irqs].global_irq = global_irq;
      arg->irqs[arg->num_irqs].active_high = active_high;
      arg->irqs[arg->num_irqs].level_triggered = level_triggered;
      arg->num_irqs++;
    }

    if (port_dev_id == UINT8_MAX) {
      for (unsigned int i = 0; i < PCI_MAX_FUNCTIONS_PER_DEVICE; ++i) {
        arg->dev_pin_to_global_irq[dev_id][i][entry->Pin] = global_irq;
      }
    } else {
      arg->dev_pin_to_global_irq[port_dev_id][port_func_id][entry->Pin] = global_irq;
    }
  }

cleanup:
  if (crs_buffer.Pointer) {
    AcpiOsFree(crs_buffer.Pointer);
  }
  if (buffer.Pointer) {
    AcpiOsFree(buffer.Pointer);
  }
  return status;
}

/* @brief Find the PCI config (returns the first one found)
 *
 * @param arg The structure to populate
 *
 * @return ZX_OK on success.
 */
static zx_status_t find_pcie_config(zx_pci_init_arg_t* arg) {
  ACPI_TABLE_HEADER* raw_table = NULL;
  ACPI_STATUS status = AcpiGetTable((char*)ACPI_SIG_MCFG, 1, &raw_table);
  if (status != AE_OK) {
    printf("could not find MCFG");
    return ZX_ERR_NOT_FOUND;
  }
  ACPI_TABLE_MCFG* mcfg = (ACPI_TABLE_MCFG*)raw_table;
  ACPI_MCFG_ALLOCATION* table_start =
      reinterpret_cast<ACPI_MCFG_ALLOCATION*>(reinterpret_cast<uintptr_t>(mcfg) + sizeof(*mcfg));
  ACPI_MCFG_ALLOCATION* table_end = reinterpret_cast<ACPI_MCFG_ALLOCATION*>(
      reinterpret_cast<uintptr_t>(mcfg) + mcfg->Header.Length);
  uintptr_t table_bytes = (uintptr_t)table_end - (uintptr_t)table_start;
  if (table_bytes % sizeof(*table_start) != 0) {
    printf("MCFG has unexpected size");
    return ZX_ERR_INTERNAL;
  }
  size_t num_entries = table_end - table_start;
  if (num_entries == 0) {
    printf("MCFG has no entries");
    return ZX_ERR_NOT_FOUND;
  }
  if (num_entries > 1) {
    printf("MCFG has more than one entry, just taking the first");
  }

  size_t size_per_bus =
      PCIE_EXTENDED_CONFIG_SIZE * PCI_MAX_DEVICES_PER_BUS * PCI_MAX_FUNCTIONS_PER_DEVICE;
  int num_buses = table_start->EndBusNumber - table_start->StartBusNumber + 1;

  if (table_start->PciSegment != 0) {
    printf("Non-zero segment found");
    return ZX_ERR_NOT_SUPPORTED;
  }

  arg->addr_windows[0].cfg_space_type = PCI_CFG_SPACE_TYPE_MMIO;
  arg->addr_windows[0].has_ecam = true;
  arg->addr_windows[0].bus_start = table_start->StartBusNumber;
  arg->addr_windows[0].bus_end = table_start->EndBusNumber;

  // We need to adjust the physical address we received to align to the proper
  // bus number.
  //
  // Citation from PCI Firmware Spec 3.0:
  // For PCI-X and PCI Express platforms utilizing the enhanced
  // configuration access method, the base address of the memory mapped
  // configuration space always corresponds to bus number 0 (regardless
  // of the start bus number decoded by the host bridge).
  arg->addr_windows[0].base = table_start->Address + size_per_bus * arg->addr_windows[0].bus_start;
  // The size of this mapping is defined in the PCI Firmware v3 spec to be
  // big enough for all of the buses in this config.
  arg->addr_windows[0].size = size_per_bus * num_buses;
  arg->addr_window_count = 1;
  return ZX_OK;
}

/* @brief Device enumerator for platform_configure_pcie_legacy_irqs */
static acpi::status<> get_pcie_devices_irq(acpi::Acpi* acpi, ACPI_HANDLE object,
                                           zx_pci_init_arg_t* arg) {
  // Start with the Root's _PRT. The spec requires that one exists.
  ACPI_STATUS status = handle_prt(object, arg, UINT8_MAX, UINT8_MAX);
  if (status != AE_OK) {
    LTRACEF("Couldn't find an IRQ routing table for root\n");
    return acpi::make_status(status);
  }

  // If there are any host bridges / pcie-to-pci bridges or other ports under
  // the root then check them for PRTs as well. This is unnecessary in most
  // configurations.
  ACPI_HANDLE child = NULL;
  while ((status = AcpiGetNextObject(ACPI_TYPE_DEVICE, object, child, &child)) == AE_OK) {
    if (auto res = acpi->GetObjectInfo(child); res.is_ok()) {
      // If the object we're examining has a PCI address then use that as the
      // basis for the routing table we're inspecting.
      // Format: Acpi 6.1 section 6.1.1 "_ADR (Address)"
      uint8_t port_dev_id = (res->Address >> 16) & (PCI_MAX_DEVICES_PER_BUS - 1);
      uint8_t port_func_id = res->Address & (PCI_MAX_FUNCTIONS_PER_DEVICE - 1);

      LTRACEF("Processing _PRT for %02x.%1x (%.*s)\n", port_dev_id, port_func_id, 4,
              reinterpret_cast<char*>(&res->Name));

      // Ignore the return value of this, since if child is not a
      // root port, it will fail and we don't care.
      handle_prt(child, arg, port_dev_id, port_func_id);
    }
  }

  return acpi::ok();
}
/* @brief Find the legacy IRQ swizzling for the PCIe root bus
 *
 * @param arg The structure to populate
 *
 * @return ZX_OK on success
 */
static zx_status_t find_pci_legacy_irq_mapping(acpi::Acpi* acpi, ACPI_HANDLE object,
                                               zx_pci_init_arg_t* arg) {
  unsigned int map_len =
      sizeof(arg->dev_pin_to_global_irq) / (sizeof(**(arg->dev_pin_to_global_irq)));
  for (unsigned int i = 0; i < map_len; ++i) {
    uint32_t* flat_map = (uint32_t*)&arg->dev_pin_to_global_irq;
    flat_map[i] = ZX_PCI_NO_IRQ_MAPPING;
  }
  arg->num_irqs = 0;

  acpi::status<> status = get_pcie_devices_irq(acpi, object, arg);
  if (status.is_error()) {
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

static acpi::status<> find_pci_configs_cb(ACPI_HANDLE object, zx_pci_init_arg_t* arg) {
  size_t size_per_bus =
      PCI_BASE_CONFIG_SIZE * PCI_MAX_DEVICES_PER_BUS * PCI_MAX_FUNCTIONS_PER_DEVICE;

  // TODO(cja): This is essentially a hacky solution to deal with
  // legacy PCI on Virtualbox and GCE. When the ACPI bus driver
  // is enabled we'll be using proper binding and not need this
  // anymore.
  if (auto res = acpi::GetObjectInfo(object); res.is_ok()) {
    arg->addr_windows[0].cfg_space_type = PCI_CFG_SPACE_TYPE_PIO;
    arg->addr_windows[0].has_ecam = false;
    arg->addr_windows[0].base = 0;
    arg->addr_windows[0].bus_start = 0;
    arg->addr_windows[0].bus_end = 0;
    arg->addr_windows[0].size = size_per_bus;
    arg->addr_window_count = 1;

    return acpi::ok();
  }

  return acpi::error(AE_ERROR);
}

/* @brief Find the PCI config (returns the first one found)
 *
 * @param arg The structure to populate
 *
 * @return ZX_OK on success.
 */
static zx_status_t find_pci_config(acpi::Acpi* acpi, zx_pci_init_arg_t* arg) {
  // TODO: Although this will find every PCI legacy root, we're presently
  // hardcoding to just use the first at bus 0 dev 0 func 0 segment 0.
  return acpi->GetDevices(PCI_HID, [arg](ACPI_HANDLE device,
                                         uint32_t) { return find_pci_configs_cb(device, arg); })
                 .is_ok()
             ? ZX_OK
             : ZX_ERR_INTERNAL;
}

/* @brief Compute PCIe initialization information
 *
 * The computed initialization information can be released with free()
 *
 * @param arg Pointer to store the initialization information into
 *
 * @return ZX_OK on success
 */
zx_status_t get_pci_init_arg(acpi::Acpi* acpi, ACPI_HANDLE object, zx_pci_init_arg_t** arg,
                             uint32_t* size) {
  zx_pci_init_arg_t* res = NULL;

  // TODO(teisenbe): We assume only one ECAM window right now...
  size_t obj_size = sizeof(*res) + sizeof(res->addr_windows[0]) * 1;
  res = static_cast<zx_pci_init_arg_t*>(calloc(1, obj_size));
  if (!res) {
    return ZX_ERR_NO_MEMORY;
  }

  // First look for a PCIe root complex. If none is found, try legacy PCI.
  // This presently only cares about the first root found, multiple roots
  // will be handled when the PCI bus driver binds to roots via ACPI.
  zx_status_t status = find_pcie_config(res);
  if (status != ZX_OK) {
    printf("PCIe config not found, trying legacy PCI\n");
    status = find_pci_config(acpi, res);
    if (status != ZX_OK) {
      printf("Failed to find either PCIe or PCI config: %d\n", status);
      goto fail;
    }
  }

  // Add more detailed error handling for IRQ mapping
  status = find_pci_legacy_irq_mapping(acpi, object, res);
  if (status != ZX_OK) {
    goto fail;
  }

  *arg = res;
  *size =
      static_cast<uint32_t>(sizeof(*res) + sizeof(res->addr_windows[0]) * res->addr_window_count);
  return ZX_OK;

fail:
  free(res);
  return status;
}

struct report_current_resources_ctx {
  bool device_is_root_bridge;
  bool add_pass;
};

static ACPI_STATUS report_current_resources_resource_cb(ACPI_RESOURCE* res, void* _ctx) {
  auto* ctx = static_cast<report_current_resources_ctx*>(_ctx);

  bool is_mmio = false;
  uint64_t base = 0;
  uint64_t len = 0;
  bool add_range = false;

  if (resource_is_memory(res)) {
    resource_memory_t mem;
    zx_status_t status = resource_parse_memory(res, &mem);
    if (status != ZX_OK || mem.minimum != mem.maximum) {
      return AE_ERROR;
    }

    is_mmio = true;
    base = mem.minimum;
    len = mem.address_length;
  } else if (resource_is_address(res)) {
    resource_address_t addr;
    zx_status_t status = resource_parse_address(res, &addr);
    if (status != ZX_OK) {
      return AE_ERROR;
    }

    if (addr.resource_type == RESOURCE_ADDRESS_MEMORY) {
      is_mmio = true;
    } else if (addr.resource_type == RESOURCE_ADDRESS_IO) {
      is_mmio = false;
    } else {
      return AE_OK;
    }

    if (!addr.min_address_fixed || !addr.max_address_fixed || addr.maximum < addr.minimum) {
      printf("WARNING: ACPI found bad _CRS address entry\n");
      return AE_OK;
    }

    // We compute len from maximum rather than address_length, since some
    // implementations don't set address_length...
    base = addr.minimum;
    len = addr.maximum - base + 1;

    // PCI root bridges report downstream resources via _CRS.  Since we're
    // gathering data on acceptable ranges for PCI to use for MMIO, consider
    // non-consume-only address resources to be valid for PCI MMIO.
    if (ctx->device_is_root_bridge && !addr.consumed_only) {
      add_range = true;
    }
  } else if (resource_is_io(res)) {
    resource_io_t io;
    zx_status_t status = resource_parse_io(res, &io);
    if (status != ZX_OK) {
      return AE_ERROR;
    }

    if (io.minimum != io.maximum) {
      printf("WARNING: ACPI found bad _CRS IO entry\n");
      return AE_OK;
    }

    is_mmio = false;
    base = io.minimum;
    len = io.address_length;
  } else {
    return AE_OK;
  }

  // Ignore empty regions that are reported, and skip any resources that
  // aren't for the pass we're doing.
  if (len == 0 || add_range != ctx->add_pass) {
    return AE_OK;
  }

  if (add_range && is_mmio && base < 1024 * 1024) {
    // The PC platform defines many legacy regions below 1MB that we do not
    // want PCIe to try to map onto.
    xprintf("Skipping adding MMIO range, due to being below 1MB\n");
    return AE_OK;
  }

  xprintf("ACPI range modification: %sing %s %016lx %016lx\n", add_range ? "add" : "subtract",
          is_mmio ? "MMIO" : "PIO", base, len);

  zx_status_t status = zx_pci_add_subtract_io_range(is_mmio, base, len, add_range);
  if (status != ZX_OK) {
    if (add_range) {
      xprintf("Failed to add range: %d\n", status);
    } else {
      // If we are subtracting a range and fail, abort.  This is bad.
      return AE_ERROR;
    }
  }
  return AE_OK;
}

static acpi::status<> pci_report_current_resources_device_cb(ACPI_HANDLE object, acpi::Acpi* acpi,
                                                             report_current_resources_ctx* ctx) {
  acpi::UniquePtr<ACPI_DEVICE_INFO> info;
  if (auto res = acpi::GetObjectInfo(object); res.is_error()) {
    return res.take_error();
  } else {
    info = std::move(res.value());
  }

  ctx->device_is_root_bridge = (info->Flags & ACPI_PCI_ROOT_BRIDGE) != 0;

  ACPI_STATUS status =
      AcpiWalkResources(object, (char*)"_CRS", report_current_resources_resource_cb, ctx);
  if (status == AE_NOT_FOUND || status == AE_OK) {
    return acpi::ok();
  }
  return acpi::make_status(status);
}

/* @brief Report current resources to the kernel PCI driver
 *
 * Walks the ACPI namespace and use the reported current resources to inform
   the kernel PCI interface about what memory it shouldn't use.
 *
 * @param mmio_resource_handle The handle to pass to the kernel when talking
 * to the PCI driver.
 *
 * @return ZX_OK on success
 */
zx_status_t pci_report_current_resources(acpi::Acpi* acpi) {
  // First we search for resources to add, then we subtract out things that
  // are being consumed elsewhere.  This forces an ordering on the
  // operations so that it should be consistent, and should protect against
  // inconistencies in the _CRS methods.

  // Walk the device tree and add to the PCIe IO ranges any resources
  // "produced" by the PCI root in the ACPI namespace.
  struct report_current_resources_ctx ctx = {
      .device_is_root_bridge = false,
      .add_pass = true,
  };
  acpi::status<> status =
      acpi->GetDevices(nullptr, [ctx = &ctx, acpi](ACPI_HANDLE hnd, uint32_t) -> acpi::status<> {
        return pci_report_current_resources_device_cb(hnd, acpi, ctx);
      });
  if (status.is_error()) {
    return ZX_ERR_INTERNAL;
  }

  // Removes resources we believe are in use by other parts of the platform
  ctx = (struct report_current_resources_ctx){
      .device_is_root_bridge = false,
      .add_pass = false,
  };
  status =
      acpi->GetDevices(nullptr, [ctx = &ctx, acpi](ACPI_HANDLE hnd, uint32_t) -> acpi::status<> {
        return pci_report_current_resources_device_cb(hnd, acpi, ctx);
      });
  if (status.is_error()) {
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

// This pci_init initializes the kernel pci driver and is not compiled in at the same time as the
// userspace pci driver under development.
zx_status_t pci_init(ACPI_HANDLE object, acpi::UniquePtr<ACPI_DEVICE_INFO> info,
                     acpi::Manager* manager, fbl::Vector<pci_bdf_t> acpi_bdfs) {
  // Report current resources to kernel PCI driver
  zx_status_t status = pci_report_current_resources(manager->acpi());
  if (status != ZX_OK) {
    LTRACEF("acpi: WARNING: ACPI failed to report all current resources!\n");
  }

  char name[ACPI_NAMESEG_SIZE + 1];
  memcpy(name, &info->Name, ACPI_NAMESEG_SIZE);
  name[ACPI_NAMESEG_SIZE] = '\0';
  LTRACEF("acpi: root %s\n", name);

  // Initialize kernel PCI driver
  zx_pci_init_arg_t* arg;
  uint32_t arg_size;
  status = get_pci_init_arg(manager->acpi(), object, &arg, &arg_size);
  if (status != ZX_OK) {
    LTRACEF("acpi: error %d in get_pci_init_arg\n", status);
    return status;
  }

  status = zx_pci_init(arg, arg_size);
  if (status != ZX_OK) {
    LTRACEF("acpi: error %d in zx_pci_init\n", status);
    return status;
  }

  free(arg);

  return ZX_OK;
}

#if 0
static zx_status_t pciroot_op_get_bti(void* /*context*/, uint32_t bdf, uint32_t index,
                                      zx_handle_t* bti) {
  // The x86 IOMMU world uses PCI BDFs as the hardware identifiers, so there
  // will only be one BTI per device.
  if (index != 0) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  // For dummy IOMMUs, the bti_id just needs to be unique.  For Intel IOMMUs,
  // the bti_ids correspond to PCI BDFs.
  zx_handle_t iommu_handle;
  zx_status_t status = iommu_manager_iommu_for_bdf(bdf, &iommu_handle);
  if (status != ZX_OK) {
    return status;
  }

  status = zx_bti_create(iommu_handle, 0, bdf, bti);
  if (status == ZX_OK) {
    char name[ZX_MAX_NAME_LEN]{};
    snprintf(name, std::size(name) - 1, "kpci bti %02x:%02x.%1x", (bdf >> 8) & 0xFF,
             (bdf >> 3) & 0x1F, bdf & 0x7);
    const zx_status_t name_status =
        zx_object_set_property(*bti, ZX_PROP_NAME, name, std::size(name));
    if (name_status != ZX_OK) {
      zxlogf(WARNING, "Couldn't set name for BTI '%s': %s", name,
             zx_status_get_string(name_status));
    }
  }

  return status;
}
#endif
