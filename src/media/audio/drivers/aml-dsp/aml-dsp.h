// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_DSP_AML_DSP_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_DSP_AML_DSP_H_

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.dsp/cpp/wire.h>
#include <fidl/fuchsia.hardware.mailbox/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/bti.h>
#include <lib/zx/interrupt.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>
#include <fbl/vector.h>

enum class DspStartMode : uint8_t {
  kScpiStartMode = 0,
  kSmcStartMode,
};

namespace aml_dsp {

class AmlDsp;
namespace FidlDsp = fuchsia_hardware_dsp;

using DeviceType = ddk::Device<AmlDsp, ddk::Unbindable, ddk::Suspendable, ddk::Resumable,
                               ddk::Messageable<FidlDsp::DspDevice>::Mixin>;

class AmlDsp : public DeviceType {
 public:
  explicit AmlDsp(zx_device_t* parent, ddk::MmioBuffer dsp_addr, ddk::MmioBuffer dsp_sram_addr,
                  fidl::ClientEnd<fuchsia_hardware_clock::Clock> dsp_clk_sel,
                  fidl::ClientEnd<fuchsia_hardware_clock::Clock> dsp_clk_gate,
                  async_dispatcher_t* dispatcher)
      : DeviceType(parent),
        dsp_addr_(std::move(dsp_addr)),
        dsp_sram_addr_(std::move(dsp_sram_addr)),
        dsp_clk_sel_(std::move(dsp_clk_sel)),
        dsp_clk_gate_(std::move(dsp_clk_gate)),
        outgoing_(dispatcher),
        dispatcher_(dispatcher) {}

  ~AmlDsp() = default;
  zx_status_t Init();

  /* Load the dsp firmware to the specified address */
  zx_status_t DspLoadFw(fidl::StringView fw_name);
  /* Enable DSP clock and power on, start DSP */
  zx_status_t DspStart();
  zx_status_t DspSmcCall(uint32_t func_id, uint8_t arg1, uint32_t arg2, uint32_t arg3);
  /* Disable DSP clock and power off, stop DSP */
  zx_status_t DspStop();
  void DspSuspend();
  void DspResume();
  /* According to the SCPI protocol, call the mailbox driver to transmit commands and data */
  zx_status_t ScpiSendData(uint8_t* data, uint8_t size, uint8_t cmd);

  static zx_status_t Create(void* ctx, zx_device_t* parent);
  zx_status_t Bind();

  // Methods required by the ddk mixins.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkSuspend(ddk::SuspendTxn txn);
  void DdkResume(ddk::ResumeTxn txn);
  void DdkRelease();

  // |fidl::WireServer<fuchsia_hardware_dsp::DspDevice>|
  void LoadFirmware(LoadFirmwareRequestView request,
                    LoadFirmwareCompleter::Sync& completer) override;
  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;

 private:
  ddk::MmioBuffer dsp_addr_;
  ddk::MmioBuffer dsp_sram_addr_;
  bool dsp_start_ = false;
  bool firmware_loaded_ = false;
  zx_paddr_t hifi_base_;
  uint8_t power_dspa_;
  bool power_manage_support_ = false;
  zx::resource smc_resource_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> dsp_clk_sel_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> dsp_clk_gate_;
  fidl::WireSyncClient<fuchsia_hardware_mailbox::Device> dsp_mailbox_;

  // This is a helper class which we use to serve the outgoing directory.
  component::OutgoingDirectory outgoing_;
  fidl::ServerBindingGroup<fuchsia_hardware_dsp::DspDevice> bindings_;
  async_dispatcher_t* dispatcher_;
};

}  // namespace aml_dsp

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_DSP_AML_DSP_H_
