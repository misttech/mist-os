// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_ROOT_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_ROOT_DEVICE_H_

#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/zx/result.h>

#include <ddktl/device.h>

#include "sdio-controller-device.h"
#include "sdmmc-block-device.h"

namespace sdmmc {

class SdmmcRootDevice;
using SdmmcRootDeviceType = ddk::Device<SdmmcRootDevice, ddk::Initializable>;

class SdmmcRootDevice : public SdmmcRootDeviceType {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  zx_status_t Init();

 private:
  SdmmcRootDevice(zx_device_t* parent, const ddk::SdmmcProtocolClient& host)
      : SdmmcRootDeviceType(parent), host_(host) {}

  // Returns the SDMMC metadata with default values for any fields that are not present (or if the
  // metadata itself is not present). Returns an error if the metadata could not be decoded.
  zx::result<fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>> GetMetadata(
      fidl::AnyArena& allocator);

  const ddk::SdmmcProtocolClient host_;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_ROOT_DEVICE_H_
