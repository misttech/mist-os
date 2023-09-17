// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_DSI_DW_DSI_DW_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_DSI_DW_DSI_DW_H_

#include <fidl/fuchsia.hardware.dsi/cpp/wire.h>
#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/fidl/cpp/wire/async_binding.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/mipi-dsi/mipi-dsi.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/channel.h>
#include <unistd.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>
#include <optional>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/dsi-dw/dw-mipi-dsi-reg.h"

namespace dsi_dw {

class DsiDw;

namespace fidl_dsi = fuchsia_hardware_dsi;

using DeviceType = ddk::Device<DsiDw, ddk::Messageable<fidl_dsi::DsiBase>::Mixin>;

class DsiDw : public DeviceType, public ddk::DsiImplProtocol<DsiDw, ddk::base_protocol> {
 public:
  // Factory method called by the device manager binding code.
  static zx_status_t Create(zx_device_t* parent);

  explicit DsiDw(zx_device_t* parent, fdf::MmioBuffer mmio);

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
  bool DsiImplIsPoweredUp();
  void DsiImplReset() { DsiImplPowerDown(); }
  zx_status_t DsiImplPhyConfig(const dsi_config_t* dsi_config) { return ZX_OK; }
  void DsiImplPhyPowerUp();
  void DsiImplPhyPowerDown();
  void DsiImplPhySendCode(uint32_t code, uint32_t parameter);
  zx_status_t DsiImplPhyWaitForReady();
  void DsiImplPrintDsiRegisters();
  zx_status_t DsiImplWriteReg(uint32_t reg, uint32_t val);
  zx_status_t DsiImplReadReg(uint32_t reg, uint32_t* val);
  zx_status_t DsiImplEnableBist(uint32_t pattern);

  // fuchsia_hardware_dsi::DsiBase
  void SendCmd(SendCmdRequestView request, SendCmdCompleter::Sync& completer) override;

  // ddk::Device implementation:
  void DdkRelease();

 private:
  inline bool IsPldREmpty() TA_REQ(command_lock_);
  inline bool IsPldRFull() TA_REQ(command_lock_);
  inline bool IsPldWEmpty() TA_REQ(command_lock_);
  inline bool IsPldWFull() TA_REQ(command_lock_);
  inline bool IsCmdEmpty() TA_REQ(command_lock_);
  inline bool IsCmdFull() TA_REQ(command_lock_);
  zx_status_t WaitforFifo(uint32_t bit, bool val) TA_REQ(command_lock_);
  zx_status_t WaitforPldWNotFull() TA_REQ(command_lock_);
  zx_status_t WaitforPldWEmpty() TA_REQ(command_lock_);
  zx_status_t WaitforPldRFull() TA_REQ(command_lock_);
  zx_status_t WaitforPldRNotEmpty() TA_REQ(command_lock_);
  zx_status_t WaitforCmdNotFull() TA_REQ(command_lock_);
  zx_status_t WaitforCmdEmpty() TA_REQ(command_lock_);
  void DumpCmd(const mipi_dsi_cmd_t& cmd);
  zx_status_t GenericPayloadRead(uint32_t* data) TA_REQ(command_lock_);
  zx_status_t GenericHdrWrite(uint32_t data) TA_REQ(command_lock_);
  zx_status_t GenericPayloadWrite(uint32_t data) TA_REQ(command_lock_);
  void EnableBta() TA_REQ(command_lock_);
  void DisableBta() TA_REQ(command_lock_);
  zx_status_t WaitforBtaAck() TA_REQ(command_lock_);
  zx_status_t GenWriteShort(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t DcsWriteShort(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t GenWriteLong(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t DcsWriteShort(const fidl_dsi::wire::MipiDsiCmd& cmd,
                            fidl::VectorView<uint8_t>& txdata) TA_REQ(command_lock_);
  zx_status_t GenRead(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t SendCommand(const mipi_dsi_cmd_t& cmd);
  zx_status_t SendCommand(const fidl_dsi::wire::MipiDsiCmd& cmd, fidl::VectorView<uint8_t>& txdata,
                          fidl::VectorView<uint8_t>& response);
  zx_status_t GetColorCode(color_code_t c, bool& packed, uint8_t& code);
  zx_status_t GetVideoMode(video_mode_t v, uint8_t& mode);

  fdf::MmioBuffer dsi_mmio_;

  // Save video config to enable seamless switching between command and video modes.
  uint32_t last_vidmode_ = 0;

  // This lock is used to synchronize SendCmd issued from FIDL server and Banjo interface
  fbl::Mutex command_lock_;
};

}  // namespace dsi_dw

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_DSI_DW_DSI_DW_H_
