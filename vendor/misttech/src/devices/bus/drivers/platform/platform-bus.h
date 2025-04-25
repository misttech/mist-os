// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_BUS_H_
#define VENDOR_MISTTECH_SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_BUS_H_

#include <lib/mistos/util/btree_map.h>
#include <lib/zbi-format/board.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <fbl/macros.h>
#include <fbl/ref_ptr.h>
#include <object/handle.h>
#include <object/iommu_dispatcher.h>

class BusTransactionInitiatorDispatcher;

namespace platform_bus {

class PlatformBus;
using PlatformBusType = ddk::Device<PlatformBus, ddk::Initializable>;

// This is the main class for the platform bus driver.
class PlatformBus : public PlatformBusType {
 public:
  static zx_status_t Create(zx_device_t* parent, const char* name);

  PlatformBus(zx_device_t* parent);

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  zx_status_t GetBti(uint32_t iommu_index, uint32_t bti_id,
                     fbl::RefPtr<BusTransactionInitiatorDispatcher>* out_bti);

  // IOMMU protocol implementation.
  zx_status_t IommuGetBti(uint32_t iommu_index, uint32_t bti_id,
                          fbl::RefPtr<BusTransactionInitiatorDispatcher>* out_bti);

  struct BootItemResult {
    fbl::RefPtr<VmObject> vmo;
    uint32_t length;
  };
  // Returns ZX_ERR_NOT_FOUND when boot item wasn't found.
  zx::result<fbl::Vector<BootItemResult>> GetBootItem(uint32_t type, std::optional<uint32_t> extra);
  zx::result<fbl::Array<uint8_t>> GetBootItemArray(uint32_t type, std::optional<uint32_t> extra);

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(PlatformBus);

  zx::result<zbi_board_info_t> GetBoardInfo();
  zx_status_t Init();

  // Dummy IOMMU.
  KernelHandle<IommuDispatcher> iommu_handle_;

  zx_device_t* protocol_passthrough_ = nullptr;

  util::BTreeMap<std::pair<uint32_t, uint32_t>, KernelHandle<BusTransactionInitiatorDispatcher>>
      cached_btis_;
};

}  // namespace platform_bus

#endif  // VENDOR_MISTTECH_SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_BUS_H_
