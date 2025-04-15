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
#include <lwip/mistos/inferfaces_admin.h>
#include <lwip/netif.h>
#include <lwip/tcpip.h>
#include <netif/etharp.h>
#include <object/pci_device_dispatcher.h>
#include <virtio/block.h>
#include <virtio/virtio.h>

#include "platform/platform-bus.h"
#include "vendor/misttech/src/connectivity/ethernet/drivers/virtio/netdevice.h"
#include "vendor/misttech/src/connectivity/lib/network-device/cpp/network_device_client.h"
#include "vendor/misttech/src/connectivity/network/drivers/network-device/device/device_interface.h"
#include "vendor/misttech/src/devices/block/drivers/virtio/block.h"

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
        case VIRTIO_DEV_TYPE_BLOCK: {
          auto dev =
              virtio::CreateAndBind<virtio::BlockDevice>(platform_bus, std::move(handle), info);
          if (dev == nullptr) {
            dprintf(CRITICAL, "Failed to create block device\n");
            continue;
          }
          // we leak it to avoid device destruction
          [[maybe_unused]] auto ptr = dev.release();

          break;
        }
        case VIRTIO_DEV_TYPE_T_NETWORK:
        case VIRTIO_DEV_TYPE_NETWORK: {
          auto dev =
              virtio::CreateAndBind<virtio::NetworkDevice>(platform_bus, std::move(handle), info);
          if (dev == nullptr) {
            dprintf(CRITICAL, "Failed to create network device\n");
            continue;
          }
          auto proto = dev->device_impl_proto();
          // we leak it to avoid device destruction
          [[maybe_unused]] auto net_dev_ptr = dev.release();

          auto device_interface = network::internal::DeviceInterface::Create(&proto);
          if (device_interface.is_error()) {
            dprintf(CRITICAL, "Failed to create device interface\n");
            continue;
          }

          fbl::AllocChecker ac;
          ktl::unique_ptr<network::client::NetworkDeviceClient> client =
              ktl::make_unique<network::client::NetworkDeviceClient>(
                  &ac, std::move(device_interface.value()));
          if (!ac.check()) {
            dprintf(CRITICAL, "Failed to allocate network device client\n");
            continue;
          }

          client->GetPorts([&client](zx::result<fbl::Vector<port_id_t>> ports) {
            if (ports.is_error()) {
              dprintf(CRITICAL, "Failed to get ports\n");
              return;
            }
            dprintf(INFO, "NET: found %ld ports\n", ports.value().size());
            for (auto& port : ports.value()) {
              // Add device to lwIP.
              fbl::AllocChecker ac;
              struct netif* netif = new (&ac) struct netif;
              if (!ac.check()) {
                dprintf(CRITICAL, "NET:failed to allocate netif\n");
                return;
              }

              std::unique_ptr<lwip::mistos::State> state =
                  fbl::make_unique_checked<lwip::mistos::State>(&ac);
              if (!ac.check()) {
                dprintf(CRITICAL, "NET:failed to allocate state\n");
                return;
              }

              state->client = client.get();
              state->port_id = port;

              netif_add(netif, nullptr, nullptr, nullptr, state.release(),
                        lwip::mistos::CreateInterface, tcpip_input);
            }
          });

          [[maybe_unused]] auto client_ptr = client.release();
          break;
        }
        default:
          break;
      }
    }
  }
}

}  // namespace platform_bus

LK_INIT_HOOK(platform_bus_scan, platform_bus::platform_bus_scan, LK_INIT_LEVEL_ARCH_LATE - 1)
