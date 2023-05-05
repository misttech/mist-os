// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/mailbox/drivers/aml-pl-mailbox/aml-pl-mailbox.h"

#include <fidl/fuchsia.hardware.mailbox/cpp/markers.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/defer.h>
#include <lib/mmio/mmio-buffer.h>
#include <stdlib.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <cstdlib>
#include <vector>

namespace {

struct MboxData {
  uint64_t reserve;
  uint64_t complete;
  uint8_t data[kMboxTxSize];
} __attribute__((packed));

constexpr uint8_t kDefaultValue = 0;
constexpr uint32_t kRxPayloadOffset = 0;
constexpr uint32_t kTxPayloadOffset = 0x200;
constexpr uint32_t kSetRegister = 0x154;  // set registers offset.
constexpr uint32_t kClrRegister = 0x190;  // clr registers offset.
constexpr uint32_t kStsRegister = 0x1d0;  // sts registers offset.

constexpr uint16_t kMboxCompleteLen = 8;
constexpr uint16_t kMboxReserveLen = 8;
constexpr uint32_t kSizeMask = 0x1FF;

#define SIZE_SHIFT(val) ((val & kSizeMask) << 16)
#define BIT(pos) (1 << pos)
#define ASYNC_CMD_TAG BIT(25)

}  // namespace

namespace aml_pl_mailbox {

void AmlPlMailbox::MboxFifoWrite(uint32_t offset, const uint8_t* from, uint8_t count) {
  auto p = reinterpret_cast<const uint32_t*>(from);
  size_t i = 0;
  for (i = 0; i < ((count / 4) * 4); i += sizeof(uint32_t)) {
    dsp_payload_mmio_.Write32(*p, offset + i);
    p++;
  }

  /* remainder data need use copy for no over mem size */
  if (count % 4 != 0) {
    uint32_t rdata = 0;
    memcpy(&rdata, p, count % 4);
    dsp_payload_mmio_.Write32(rdata, offset + i);
  }
}

void AmlPlMailbox::MboxFifoClr(uint32_t offset) {
  for (size_t i = 0; i < kMboxFifoSize; i += sizeof(uint32_t)) {
    dsp_payload_mmio_.Write32(0, offset + i);
  }
}

int AmlPlMailbox::DspRecvIrqThread() {
  while (1) {
    zx_status_t status = dsp_recv_irq_.wait(nullptr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "dsp recv irq wait failed, retcode %s", zx_status_get_string(status));
      return status;
    }

    uint32_t irq_status = dsp_mmio_.Read32(kStsRegister);
    // mbox dsp handler.
    if (irq_status) {
      dsp_payload_mmio_.ReadBuffer(kRxPayloadOffset, channels_.data(), channels_.size());
      dsp_mmio_.Write32(~0, kClrRegister);
      rx_flag_ = 1;
    } else {
      // mbox ack isr handler.
      dsp_payload_mmio_.ReadBuffer(kTxPayloadOffset, channels_.data(), channels_.size());
    }
  }
  return ZX_OK;
}

void AmlPlMailbox::MailboxSendData(uint32_t cmd, uint8_t* data, uint8_t tx_size) {
  MboxFifoClr(kTxPayloadOffset);
  MboxFifoWrite(kTxPayloadOffset, data, tx_size);
  dsp_mmio_.Write32(cmd, kSetRegister);
}

zx_status_t AmlPlMailbox::MboxMessageWrite(const FidlMailbox::wire::MboxTx& mdata) {
  uint8_t tx_size = static_cast<uint8_t>(mdata.tx_buffer.count());
  if (tx_size > kMboxTxSize) {
    zxlogf(ERROR, "Message length is out of range.");
    return ZX_ERR_OUT_OF_RANGE;
  }

  tx_size = tx_size + kMboxCompleteLen + kMboxReserveLen;
  uint32_t new_cmd = (mdata.cmd & UINT32_MAX) | SIZE_SHIFT(tx_size - kMboxReserveLen) |
                     static_cast<uint32_t>(ASYNC_CMD_TAG);

  /* This uses a mailbox communication mechanism to send data once. The complete data structure
   * should be: uint64_t reserve + uint64_t complete + the actual data sent. According to the
   * mechanism, when ARM sends data, the reserve needs to be set to 0, complete is set to 0.
   * Therefore, the first 16 bytes of the data array are set to 0, and the actual sent data starts
   * from data[16]. */
  MboxData mboxdata = {
      .reserve = kDefaultValue,
      .complete = kDefaultValue,
  };

  memset(mboxdata.data, 0, sizeof(mboxdata.data));
  memcpy(mboxdata.data, &mdata.tx_buffer[0], mdata.tx_buffer.count() * sizeof(mdata.tx_buffer[0]));
  MailboxSendData(new_cmd, reinterpret_cast<uint8_t*>(&mboxdata), tx_size);
  return ZX_OK;
}

void AmlPlMailbox::ReceiveData(ReceiveDataRequestView request,
                               ReceiveDataCompleter::Sync& completer) {
  using fuchsia_hardware_mailbox::wire::DeviceReceiveDataResponse;

  while (rx_flag_ == 0)
    ;

  fidl::Arena allocator;
  DeviceReceiveDataResponse response;
  response.mdata.rx_buffer.Allocate(allocator, request->rx_len);

  /* This is a mailbox communication mechanism to receive data once, and the complete data structure
   * should be: uint64_t reserve + uint64_t complete  + actual data received, according to the
   * mechanism, the data required by the user is from channels_[16] started. */
  memcpy(&response.mdata.rx_buffer[0], &channels_[kMboxCompleteLen + kMboxReserveLen],
         response.mdata.rx_buffer.count() * sizeof(response.mdata.rx_buffer[0]));

  rx_flag_ = 0;
  memset(channels_.data(), 0, channels_.size());

  completer.Reply(::fit::ok(&response));
}

void AmlPlMailbox::SendCommand(SendCommandRequestView request,
                               SendCommandCompleter::Sync& completer) {
  FidlMailbox::wire::MboxTx data = request->mdata;
  uint8_t channel = request->channel;
  ZX_DEBUG_ASSERT(channel >= 0);
  ZX_DEBUG_ASSERT(channel <= kMboxMax);

  zx_status_t status = MboxMessageWrite(data);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess();
  }
}

void AmlPlMailbox::ShutDown() {
  dsp_recv_irq_.destroy();
  thrd_join(dsp_recv_irq_thread_, nullptr);
}

void AmlPlMailbox::DdkUnbind(ddk::UnbindTxn txn) {
  ShutDown();
  dsp_mmio_.reset();
  dsp_payload_mmio_.reset();
  txn.Reply();
}

void AmlPlMailbox::DdkRelease() { delete this; }

zx_status_t AmlPlMailbox::Init() {
  memset(channels_.data(), 0, channels_.size());

  auto thunk = [](void* arg) -> int {
    return reinterpret_cast<AmlPlMailbox*>(arg)->DspRecvIrqThread();
  };
  int rc = thrd_create_with_name(&dsp_recv_irq_thread_, thunk, this, "dsp_recv_irq_");
  if (rc != thrd_success) {
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

AmlPlMailbox::~AmlPlMailbox() {
  if (dsp_recv_irq_.is_valid()) {
    ShutDown();
  }
}

zx_status_t AmlPlMailbox::Bind() {
  zx::result status = outgoing_.AddService<fuchsia_hardware_mailbox::Service>(
      fuchsia_hardware_mailbox::Service::InstanceHandler({
          .device = bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure),
      }));
  if (status.is_error()) {
    zxlogf(ERROR, "failed to add FIDL protocol to the outgoing directory: %s",
           status.status_string());
    return status.status_value();
  }

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  status = outgoing_.Serve(std::move(endpoints->server));
  std::array offers = {
      fuchsia_hardware_mailbox::Service::Name,
  };

  char device_name[32] = {};
  snprintf(device_name, sizeof(device_name), "aml-pl-mailbox%d", mailbox_id_);

  zx_device_prop_t props[] = {
      {BIND_MAILBOX_ID, 0, mailbox_id_},
  };

  zx_status_t add_status = DdkAdd(ddk::DeviceAddArgs(device_name)
                                      .set_flags(DEVICE_ADD_MUST_ISOLATE)
                                      .set_props(props)
                                      .set_fidl_service_offers(offers)
                                      .set_outgoing_dir(endpoints->client.TakeChannel())
                                      .set_proto_id(ZX_PROTOCOL_AML_MAILBOX));

  if (add_status != ZX_OK) {
    zxlogf(ERROR, "Failed to bind FIDL device");
  }
  return add_status;
}

zx_status_t AmlPlMailbox::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;
  fbl::AllocChecker ac;
  ddk::PDevFidl pdev(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Failed to get ZX_PROTOCOL_PDEV");
    return ZX_ERR_NO_RESOURCES;
  }

  pdev_device_info_t info;
  if ((status = pdev.GetDeviceInfo(&info)) != ZX_OK) {
    zxlogf(ERROR, "pdev_get_device_info failed %s", zx_status_get_string(status));
    return ZX_ERR_NOT_SUPPORTED;
  }

  std::optional<fdf::MmioBuffer> dsp_mmio;
  if ((status = pdev.MapMmio(0, &dsp_mmio)) != ZX_OK) {
    zxlogf(ERROR, "pdev_map_mmio_buffer dsp failed %s", zx_status_get_string(status));
    return status;
  }

  std::optional<fdf::MmioBuffer> dsp_payload_mmio;
  if ((status = pdev.MapMmio(1, &dsp_payload_mmio)) != ZX_OK) {
    zxlogf(ERROR, "pdev_map_mmio_buffer dspa_payload failed %s", zx_status_get_string(status));
    return status;
  }

  zx::interrupt dsp_recv_irq;
  if ((status = pdev.GetInterrupt(0, &dsp_recv_irq)) != ZX_OK) {
    return status;
  }

  uint8_t mailbox_id = 0;
  if (!strncmp(info.name, "mailbox1", 8)) {
    mailbox_id = 1;
  }

  async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  auto dev = fbl::make_unique_checked<AmlPlMailbox>(
      &ac, parent, *std::move(dsp_mmio), *std::move(dsp_payload_mmio), std::move(dsp_recv_irq),
      mailbox_id, dispatcher);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  status = dev->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "AmlPlMailbox initialization failed %s", zx_status_get_string(status));
    return status;
  }

  status = dev->Bind();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Bind() failed for FIDL device: %s\n", zx_status_get_string(status));
    return status;
  }
  [[maybe_unused]] auto dummy = dev.release();

  return status;
}

static zx_driver_ops_t mailbox_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = AmlPlMailbox::Create,
};

}  // namespace aml_pl_mailbox

ZIRCON_DRIVER(aml_pl_mailbox, aml_pl_mailbox::mailbox_driver_ops, "zircon", "0.1");
