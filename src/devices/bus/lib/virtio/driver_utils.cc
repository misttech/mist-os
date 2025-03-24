// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/virtio/driver_utils.h"

#include <lib/virtio/backends/pci.h>
#include <lib/zx/result.h>
#include <trace.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <memory>

#include <dev/pcie_device.h>
#include <object/bus_transaction_initiator_dispatcher.h>
#include <object/pci_device_dispatcher.h>
#include <platform/platform-bus.h>

#include <ktl/enforce.h>

namespace virtio {

zx::result<
    ktl::pair<fbl::RefPtr<BusTransactionInitiatorDispatcher>, ktl::unique_ptr<virtio::Backend>>>
GetBtiAndBackend(platform_bus::PlatformBus* platform_bus, KernelHandle<PciDeviceDispatcher> pci,
                 zx_pcie_device_info_t info) {
  if (!pci.dispatcher()) {
    dprintf(CRITICAL, "pci client invalid");
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  fbl::RefPtr<BusTransactionInitiatorDispatcher> bti;
  zx_status_t status = platform_bus->GetBti(0, 0, &bti);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  // Due to the similarity between Virtio 0.9.5 legacy devices and Virtio 1.0
  // transitional devices we need to check whether modern capabilities exist.
  // If no vendor capabilities are found then we will default to the legacy
  // interface.
  ktl::unique_ptr<virtio::Backend> backend = nullptr;

  auto device = pci.dispatcher()->device();
  auto cap = device->capabilities().find_if(
      [](const auto& cap) { return cap.id() == PCIE_CAP_ID_VENDOR; });

  bool is_modern = cap != device->capabilities().end();

  if (is_modern) {
    fbl::AllocChecker ac;
    backend = ktl::make_unique<virtio::PciModernBackend>(&ac, ktl::move(pci), info);
    if (!ac.check()) {
      dprintf(CRITICAL, "Failed to allocate memory for PCI backend\n");
      return zx::error(ZX_ERR_NO_MEMORY);
    }
  } else {
    dprintf(CRITICAL, "virtio PCI cap not found, legacy mode not supported\n");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  dprintf(INFO, "virtio %02x:%02x.%1x using %s PCI backend\n", info.bus_id, info.dev_id,
          info.func_id, (is_modern) ? "modern" : "legacy");

  status = backend->Bind();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(ktl::pair(ktl::move(bti), ktl::move(backend)));
}

}  // namespace virtio
