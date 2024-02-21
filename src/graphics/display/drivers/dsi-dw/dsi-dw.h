// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_DSI_DW_DSI_DW_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_DSI_DW_DSI_DW_H_

#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <lib/mmio/mmio.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

#include "src/graphics/display/lib/designware-dsi/dsi-host-controller.h"

namespace dsi_dw {

class DsiDw;

using DeviceType = ddk::Device<DsiDw>;

class DsiDw : public DeviceType, public ddk::DsiImplProtocol<DsiDw, ddk::base_protocol> {
 public:
  // Factory method called by the device manager binding code.
  static zx_status_t Create(zx_device_t* parent);

  explicit DsiDw(zx_device_t* parent, fdf::MmioBuffer dsi_mmio);

  DsiDw(const DsiDw&) = delete;
  DsiDw(DsiDw&&) = delete;
  DsiDw& operator=(const DsiDw&) = delete;
  DsiDw& operator=(DsiDw&&) = delete;

  ~DsiDw();

  // DsiImplProtocol implementation:
  zx_status_t DsiImplConfig(const dsi_config_t* dsi_config);
  void DsiImplPowerUp();
  void DsiImplPowerDown();
  void DsiImplSetMode(dsi_mode_t mode);
  zx_status_t DsiImplSendCmd(const mipi_dsi_cmd_t* cmd_list, size_t cmd_count);
  void DsiImplPhyPowerUp();
  void DsiImplPhyPowerDown();
  void DsiImplPhySendCode(uint32_t code, uint32_t parameter);
  zx_status_t DsiImplPhyWaitForReady();

  // ddk::Device implementation:
  void DdkRelease();

 private:
  designware_dsi::DsiHostController dsi_host_controller_;
};

}  // namespace dsi_dw

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_DSI_DW_DSI_DW_H_
