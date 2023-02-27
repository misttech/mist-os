// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_MAILBOX_DRIVERS_AML_PL_MAILBOX_AML_PL_MAILBOX_H_
#define SRC_DEVICES_MAILBOX_DRIVERS_AML_PL_MAILBOX_AML_PL_MAILBOX_H_

#include <fidl/fuchsia.hardware.mailbox/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>
#include <fbl/vector.h>

namespace {

constexpr uint16_t kMboxFifoSize = 512;
constexpr uint16_t kMboxTxSize = 244;
constexpr uint8_t kMboxMax = 2;

}  // namespace

namespace aml_pl_mailbox {

class AmlPlMailbox;
namespace FidlMailbox = fuchsia_hardware_mailbox;

using DeviceType =
    ddk::Device<AmlPlMailbox, ddk::Unbindable, ddk::Messageable<FidlMailbox::Device>::Mixin>;

class AmlPlMailbox : public DeviceType {
 public:
  AmlPlMailbox(zx_device_t* parent, fdf::MmioBuffer dsp_mmio, fdf::MmioBuffer dsp_payload_mmio,
               zx::interrupt dsp_recv_irq, uint8_t mailbox_id, async_dispatcher_t* dispatcher)
      : DeviceType(parent),
        dsp_mmio_(std::move(dsp_mmio)),
        dsp_payload_mmio_(std::move(dsp_payload_mmio)),
        dsp_recv_irq_(std::move(dsp_recv_irq)),
        mailbox_id_(mailbox_id),
        outgoing_(dispatcher),
        dispatcher_(dispatcher) {}

  ~AmlPlMailbox();
  zx_status_t Init();
  zx_status_t Bind();
  void ShutDown();
  int DspRecvIrqThread();

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void MboxFifoWrite(uint32_t offset, const uint8_t* from, uint8_t count);
  void MboxFifoClr(uint32_t offset);

  // Methods required by the ddk mixins.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void MailboxSendData(uint32_t cmd, uint8_t* data, uint8_t tx_size);
  zx_status_t MboxMessageWrite(const FidlMailbox::wire::MboxTx& mdata);

  // |fidl::WireServer<fuchsia_hardware_mailbox::Device>|
  void SendCommand(SendCommandRequestView request, SendCommandCompleter::Sync& completer) override;
  void ReceiveData(ReceiveDataRequestView request, ReceiveDataCompleter::Sync& completer) override;

 private:
  fdf::MmioBuffer dsp_mmio_;
  fdf::MmioBuffer dsp_payload_mmio_;
  zx::interrupt dsp_recv_irq_;
  thrd_t dsp_recv_irq_thread_;
  uint8_t rx_flag_ = 0;
  uint8_t mailbox_id_ = 0;
  std::array<uint8_t, kMboxFifoSize> channels_;

  fidl::ServerBindingGroup<fuchsia_hardware_mailbox::Device> bindings_;
  component::OutgoingDirectory outgoing_;
  async_dispatcher_t* dispatcher_;
};

}  // namespace aml_pl_mailbox

#endif  //  SRC_DEVICES_MAILBOX_DRIVERS_AML_PL_MAILBOX_AML_PL_MAILBOX_H_
