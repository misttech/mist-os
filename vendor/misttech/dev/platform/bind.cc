// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/virtio/backends/pci.h>
#include <lib/virtio/driver_utils.h>
#include <trace.h>
#include <zircon/errors.h>

#include <dev/pcie_bus_driver.h>
#include <dev/pcie_device.h>
#include <kernel/thread.h>
#include <ktl/unique_ptr.h>
#include <lk/init.h>
#include <object/pci_device_dispatcher.h>
#include <virtio/block.h>
#include <virtio/virtio.h>

#include "platform/platform-bus.h"
#include "src/connectivity/ethernet/drivers/virtio/netdevice.h"
#include "src/devices/block/drivers/virtio/block.h"

namespace platform_bus {

namespace {
PlatformBus* platform_bus;
}

void platform_bus_scan(uint level) {
  ZX_ASSERT(PlatformBus::Create("platform-bus", &platform_bus) == ZX_OK);

  auto bus_drv = PcieBusDriver::GetDriver();
  if (bus_drv == nullptr) {
    dprintf(CRITICAL, "pci bus not found\n");
    return;
  }

  for (uint index = 0;; index++) {
    auto device = bus_drv->GetNthDevice(index);
    if (device == nullptr) {
      break;
    }

    if (device->vendor_id() == VIRTIO_PCI_VENDOR_ID) {
      KernelHandle<PciDeviceDispatcher> handle;
      zx_pcie_device_info_t info;
      zx_rights_t rights;
      zx_status_t status = PciDeviceDispatcher::Create(index, &info, &handle, &rights);
      if (status != ZX_OK) {
        dprintf(CRITICAL, "Failed to create PCI device: %d\n", status);
        continue;
      }

      switch (device->device_id()) {
        case VIRTIO_DEV_TYPE_T_BLOCK:
        case VIRTIO_DEV_TYPE_BLOCK:
          virtio::CreateAndBind<virtio::BlockDevice>(platform_bus, std::move(handle), info);
          break;
        case VIRTIO_DEV_TYPE_T_NETWORK:
        case VIRTIO_DEV_TYPE_NETWORK:
          virtio::CreateAndBind<virtio::NetworkDevice>(platform_bus, std::move(handle), info);
          break;
        default:
          break;
      }
    }
  }
}

}  // namespace platform_bus

LK_INIT_HOOK(platform_bus_scan, platform_bus::platform_bus_scan, LK_INIT_LEVEL_ARCH_LATE - 1)
