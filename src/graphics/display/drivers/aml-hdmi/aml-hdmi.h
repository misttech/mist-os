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
#include <lib/hdmi-dw/hdmi-dw.h>
#include <lib/hdmi/base.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <cstdint>
#include <cstring>
#include <optional>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_counted.h>

namespace aml_hdmi {

class AmlHdmiDevice;
using DeviceType = ddk::Device<AmlHdmiDevice, ddk::Messageable<fuchsia_hardware_hdmi::Hdmi>::Mixin,
                               ddk::Unbindable>;

class AmlHdmiDevice : public DeviceType, public HdmiIpBase, public fbl::RefCounted<AmlHdmiDevice> {
 public:
  // Factory function called by the device manager binding code.
  static zx_status_t Create(zx_device_t* parent);

  explicit AmlHdmiDevice(zx_device_t* parent);

  // For unit testing
  //
  // `mmio` is the region documented as HDMITX in Section 8.1 "Memory Map" of
  // the AMLogic A311D datasheet.
  AmlHdmiDevice(zx_device_t* parent, fdf::MmioBuffer hdmitx_mmio,
                std::unique_ptr<hdmi_dw::HdmiDw> hdmi_dw);

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
    completer.Reply(is_powered_up_);
  }
  void Reset(ResetRequestView request, ResetCompleter::Sync& completer) override;
  void ModeSet(ModeSetRequestView request, ModeSetCompleter::Sync& completer) override;
  void EdidTransfer(EdidTransferRequestView request,
                    EdidTransferCompleter::Sync& completer) override;
  void WriteReg(WriteRegRequestView request, WriteRegCompleter::Sync& completer) override {
    WriteReg(request->reg, request->val);
    completer.Reply();
  }
  void ReadReg(ReadRegRequestView request, ReadRegCompleter::Sync& completer) override {
    auto val = ReadReg(request->reg);
    completer.Reply(val);
  }
  void EnableBist(EnableBistRequestView request, EnableBistCompleter::Sync& completer) override {
    ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
    completer.ReplySuccess();
  }
  void PrintHdmiRegisters(PrintHdmiRegistersCompleter::Sync& completer) override;

 private:
  enum {
    MMIO_HDMI,
  };

  void WriteIpReg(uint32_t addr, uint32_t data) override {
    fbl::AutoLock lock(&register_lock_);
    hdmitx_mmio_->Write8(data, addr);
  }
  uint32_t ReadIpReg(uint32_t addr) override {
    fbl::AutoLock lock(&register_lock_);
    return hdmitx_mmio_->Read8(addr);
  }
  void WriteReg(uint32_t reg, uint32_t val);
  uint32_t ReadReg(uint32_t reg);

  void PrintRegister(const char* register_name, uint32_t register_address);

  ddk::PDevFidl pdev_;
  fbl::Mutex dw_lock_;
  std::unique_ptr<hdmi_dw::HdmiDw> hdmi_dw_ TA_GUARDED(dw_lock_);

  fbl::Mutex register_lock_;
  std::optional<fdf::MmioBuffer> hdmitx_mmio_ TA_GUARDED(register_lock_);

  bool is_powered_up_ = false;

  std::optional<component::OutgoingDirectory> outgoing_;
  fidl::ServerBindingGroup<fuchsia_hardware_hdmi::Hdmi> bindings_;

  async::Loop loop_;
};

}  // namespace aml_hdmi

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AML_HDMI_AML_HDMI_H_
