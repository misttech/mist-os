// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "platform/platform-bus.h"

#include <lib/zbi-format/board.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <stdlib.h>
#include <trace.h>
#include <zircon/status.h>
#include <zircon/syscalls/iommu.h>

#include <object/bus_transaction_initiator_dispatcher.h>

#define LOCAL_TRACE 0

namespace platform_bus {

zx_status_t PlatformBus::IommuGetBti(uint32_t iommu_index, uint32_t bti_id,
                                     fbl::RefPtr<BusTransactionInitiatorDispatcher>* out_bti) {
  if (iommu_index != 0) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::pair key(iommu_index, bti_id);
  auto bti = cached_btis_.find(key);
  if (bti == cached_btis_.end()) {
    KernelHandle<BusTransactionInitiatorDispatcher> new_bti;
    zx_rights_t rights;
    zx_status_t status = BusTransactionInitiatorDispatcher::Create(iommu_handle_.dispatcher(),
                                                                   bti_id, &new_bti, &rights);
    if (status != ZX_OK) {
      return status;
    }

    auto [iter, _] = cached_btis_.emplace(key, std::move(new_bti));
    bti = iter;
  }

  *out_bti = bti->second.dispatcher();

  return ZX_OK;
}

zx_status_t PlatformBus::Create(const char* name, PlatformBus** out_platform_bus) {
  fbl::AllocChecker ac;
  platform_bus::PlatformBus* bus = new (&ac) platform_bus::PlatformBus();
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (zx::result<> status = bus->Start(); status.is_error()) {
    LTRACEF("failed to init: %d", status.error_value());
    return status.error_value();
  }

  *out_platform_bus = bus;

  return ZX_OK;
}

zx_status_t PlatformBus::GetBti(uint32_t iommu_index, uint32_t bti_id,
                                fbl::RefPtr<BusTransactionInitiatorDispatcher>* out_bti) {
  fbl::RefPtr<BusTransactionInitiatorDispatcher> bti;
  zx_status_t status = IommuGetBti(iommu_index, bti_id, &bti);

  if (status != ZX_OK) {
    return ZX_ERR_NOT_FOUND;
  }

  *out_bti = std::move(bti);
  return ZX_OK;
}

zx::result<fbl::Vector<PlatformBus::BootItemResult>> PlatformBus::GetBootItem(
    uint32_t type, std::optional<uint32_t> extra) {
  // fbl::Vector<PlatformBus::BootItemResult> ret;
  //  ret.reserve(items.count());
  // return zx::ok(std::move(ret));
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<fbl::Array<uint8_t>> PlatformBus::GetBootItemArray(uint32_t type,
                                                              std::optional<uint32_t> extra) {
  zx::result result = GetBootItem(type, extra);
  if (result.is_error()) {
    return result.take_error();
  }
  if (result->size() > 1) {
    LTRACEF("Found multiple boot items of type: %u\n", type);
  }
  auto& [vmo, length] = result.value()[0];
  fbl::AllocChecker ac;
  fbl::Array<uint8_t> data(new (&ac) uint8_t[length], length);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  zx_status_t status = vmo->Read(data.data(), 0, data.size());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(data));
}

zx::result<> PlatformBus::Start() {
  fbl::AllocChecker ac;

  // Set up a dummy IOMMU protocol to use in the case where our board driver
  // does not set a real one.
  zx_iommu_desc_dummy_t desc;

  zx_rights_t rights;
  size_t desc_size = sizeof(desc);
  ktl::unique_ptr<uint8_t[]> copied_desc(new (&ac) uint8_t[desc_size]);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  memcpy(copied_desc.get(), &desc, desc_size);
  zx_status_t status = IommuDispatcher::Create(ZX_IOMMU_TYPE_DUMMY, std::move(copied_desc),
                                               desc_size, &iommu_handle_, &rights);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  // Read platform ID.
  zx::result platform_id_result = GetBootItem(ZBI_TYPE_PLATFORM_ID, {});
  if (platform_id_result.is_error() && platform_id_result.status_value() != ZX_ERR_NOT_FOUND) {
    return platform_id_result.take_error();
  }

#if __aarch64__
  {
    // For arm64, we do not expect a board to set the bootloader info.
    bootloader_info_.vendor() = "<unknown>";
  }
#endif

  if (platform_id_result.is_ok()) {
    if (platform_id_result.value()[0].length != sizeof(zbi_platform_id_t)) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    zbi_platform_id_t platform_id;
    zx_status_t s = platform_id_result.value()[0].vmo->Read(&platform_id, 0, sizeof(platform_id));
    if (s != ZX_OK) {
      return zx::error(s);
    }
    dprintf(INFO, "VID: %u PID: %u board: \"%s\"\n", platform_id.vid, platform_id.pid,
            platform_id.board_name);
    //  fdf::info("VID: {} PID: {} board: \"{}\"", platform_id.vid, platform_id.pid,
    //            std::string_view(platform_id.board_name));
    //  board_info_.vid() = platform_id.vid;
    //  board_info_.pid() = platform_id.pid;
    //  board_info_.board_name() = platform_id.board_name;
  } else {
#if __x86_64__
    // For x64, we might not find the ZBI_TYPE_PLATFORM_ID, old bootloaders
    // won't support this, for example. If this is the case, cons up the VID/PID
    // here to allow the acpi board driver to load and bind.
    // board_info_.vid() = PDEV_VID_INTEL;
    // board_info_.pid() = PDEV_PID_X86;
#else
    dprintf(ERROR, "ZBI_TYPE_PLATFORM_ID not found");
    return zx::error(ZX_ERR_INTERNAL);
#endif
  }

  // Set default board_revision.
  zx::result zbi_board_info = GetBoardInfo();
  if (zbi_board_info.is_ok()) {
    // board_info_.board_revision() = zbi_board_info->revision;
  }

  zx::result board_data = GetBootItemArray(ZBI_TYPE_DRV_BOARD_PRIVATE, {});
  if (board_data.is_error() && board_data.status_value() != ZX_ERR_NOT_FOUND) {
    return board_data.take_error();
  }

  return zx::ok();
}

zx::result<zbi_board_info_t> PlatformBus::GetBoardInfo() {
  zx::result result = GetBootItem(ZBI_TYPE_DRV_BOARD_INFO, {});
  if (result.is_error()) {
    // This is expected on some boards.
    dprintf(INFO, "Boot Item ZBI_TYPE_DRV_BOARD_INFO not found\n");
    return result.take_error();
  }
  auto& [vmo, length] = result.value()[0];
  if (length != sizeof(zbi_board_info_t)) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  zbi_board_info_t board_info;
  zx_status_t status = vmo->Read(&board_info, 0, length);
  if (status != ZX_OK) {
    dprintf(CRITICAL, "Failed to read zbi_board_info_t VMO: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(board_info);
}

}  // namespace platform_bus
