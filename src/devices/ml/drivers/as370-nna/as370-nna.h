// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_ML_DRIVERS_AS370_NNA_AS370_NNA_H_
#define SRC_DEVICES_ML_DRIVERS_AS370_NNA_AS370_NNA_H_

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <zircon/fidl.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

namespace as370_nna {

class As370NnaDevice;
using As370NnaDeviceType = ddk::Device<As370NnaDevice>;

class As370NnaDevice : public As370NnaDeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_NNA> {
 public:
  explicit As370NnaDevice(zx_device_t* parent,
                          fidl::WireSyncClient<fuchsia_hardware_registers::Device> global_registers,
                          ddk::PDevFidl pdev)
      : As370NnaDeviceType(parent),
        pdev_(std::move(pdev)),
        global_registers_(std::move(global_registers)) {}
  static zx_status_t Create(void* ctx, zx_device_t* parent);
  zx_status_t Init();

  void DdkRelease();

 private:
  ddk::PDevFidl pdev_;
  fidl::WireSyncClient<fuchsia_hardware_registers::Device> global_registers_;
};

}  // namespace as370_nna

#endif  // SRC_DEVICES_ML_DRIVERS_AS370_NNA_AS370_NNA_H_
