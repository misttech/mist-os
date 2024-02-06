// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_RPMB_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_RPMB_DEVICE_H_

#include <fidl/fuchsia.hardware.rpmb/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/sdmmc/hw.h>

#include <array>
#include <cinttypes>
#include <optional>

namespace sdmmc {

class SdmmcBlockDevice;

class RpmbDevice : public fidl::WireServer<fuchsia_hardware_rpmb::Rpmb> {
 public:
  static constexpr char kDeviceName[] = "rpmb";

  // sdmmc_parent is owned by the SDMMC root device when the RpmbDevice object is created. Ownership
  // is transferred to devmgr shortly after, meaning it will outlive this object due to the
  // parent/child device relationship.
  RpmbDevice(SdmmcBlockDevice* sdmmc_parent, const std::array<uint8_t, SDMMC_CID_SIZE>& cid,
             const std::array<uint8_t, MMC_EXT_CSD_SIZE>& ext_csd)
      : sdmmc_parent_(sdmmc_parent),
        cid_(cid),
        rpmb_size_(ext_csd[MMC_EXT_CSD_RPMB_SIZE_MULT]),
        reliable_write_sector_count_(ext_csd[MMC_EXT_CSD_REL_WR_SEC_C]) {}

  zx_status_t AddDevice();

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override;
  void Request(RequestRequestView request, RequestCompleter::Sync& completer) override;

  fdf::Logger& logger();

 private:
  void Serve(fidl::ServerEnd<fuchsia_hardware_rpmb::Rpmb> request);

  SdmmcBlockDevice* const sdmmc_parent_;
  const std::array<uint8_t, SDMMC_CID_SIZE> cid_;
  const uint8_t rpmb_size_;
  const uint8_t reliable_write_sector_count_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_RPMB_DEVICE_H_
