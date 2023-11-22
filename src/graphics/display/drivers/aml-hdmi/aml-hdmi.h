// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AML_HDMI_AML_HDMI_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AML_HDMI_AML_HDMI_H_

#include <fidl/fuchsia.hardware.hdmi/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <cstdint>
#include <cstring>
#include <optional>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/lib/amlogic-hdmitx/amlogic-hdmitx.h"

namespace aml_hdmi {

class AmlHdmiDevice;
using DeviceType = ddk::Device<AmlHdmiDevice, ddk::Messageable<fuchsia_hardware_hdmi::Hdmi>::Mixin,
                               ddk::Unbindable>;

class AmlHdmiDevice : public DeviceType {
 public:
  // Factory function called by the device manager binding code.
  static zx_status_t Create(zx_device_t* parent);

  explicit AmlHdmiDevice(zx_device_t* parent);

  // Creates a AmlHdmiDevice with resources injected for unit testing.
  //
  // `hdmitx_top_level_mmio` is the top-level register sub-region of the
  // HDMITX MMIO register region.
  //
  // The HDMITX register region is defined in Section 8.1 "Memory Map" of
  // the AMLogic A311D datasheet. The sub-region is defined in Section
  // 10.2.3.43 "HDMITX Top-Level and HDMI TX Contoller IP Register Access" of
  // the AMLogic A311D datasheet.
  AmlHdmiDevice(zx_device_t* parent, fdf::MmioBuffer hdmitx_top_level_mmio,
                std::unique_ptr<designware_hdmi::HdmiTransmitterController> hdmi_dw,
                zx::resource smc);

  AmlHdmiDevice(const AmlHdmiDevice&) = delete;
  AmlHdmiDevice(AmlHdmiDevice&&) = delete;
  AmlHdmiDevice& operator=(const AmlHdmiDevice&) = delete;
  AmlHdmiDevice& operator=(AmlHdmiDevice&&) = delete;

  ~AmlHdmiDevice() override;

  zx_status_t Bind();

  void DdkUnbind(ddk::UnbindTxn txn) {
    loop_.Shutdown();
    txn.Reply();
  }
  void DdkRelease() { delete this; }

  void PowerUp(PowerUpRequestView request, PowerUpCompleter::Sync& completer) override {
    ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
    // no-op. initialization handled in modeset
    completer.ReplySuccess();
  }
  void PowerDown(PowerDownRequestView request, PowerDownCompleter::Sync& completer) override {
    // no-op. handled by phy
    ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
    completer.Reply();
  }
  void IsPoweredUp(IsPoweredUpRequestView request, IsPoweredUpCompleter::Sync& completer) override {
    ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
    completer.Reply(true);
  }
  void Reset(ResetRequestView request, ResetCompleter::Sync& completer) override;
  void ModeSet(ModeSetRequestView request, ModeSetCompleter::Sync& completer) override;
  void EdidTransfer(EdidTransferRequestView request,
                    EdidTransferCompleter::Sync& completer) override;
  void WriteReg(WriteRegRequestView request, WriteRegCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void ReadReg(ReadRegRequestView request, ReadRegCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void EnableBist(EnableBistRequestView request, EnableBistCompleter::Sync& completer) override {
    ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
    completer.ReplySuccess();
  }
  void PrintHdmiRegisters(PrintHdmiRegistersCompleter::Sync& completer) override;

 private:
  ddk::PDevFidl pdev_;

  std::unique_ptr<amlogic_display::HdmiTransmitter> hdmi_transmitter_;

  std::optional<component::OutgoingDirectory> outgoing_;
  fidl::ServerBindingGroup<fuchsia_hardware_hdmi::Hdmi> bindings_;

  async::Loop loop_;
};

}  // namespace aml_hdmi

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AML_HDMI_AML_HDMI_H_
