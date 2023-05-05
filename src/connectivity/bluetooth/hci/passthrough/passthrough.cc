// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <fuchsia/hardware/bt/hci/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>

#include <ddktl/device.h>
#include <ddktl/fidl.h>

namespace {

class PassthroughDevice;

using PassthroughDeviceType = ddk::Device<PassthroughDevice, ddk::GetProtocolable,
                                          ddk::Messageable<fuchsia_hardware_bluetooth::Hci>::Mixin>;

class PassthroughDevice : public PassthroughDeviceType {
 public:
  explicit PassthroughDevice(zx_device_t* parent) : PassthroughDeviceType(parent) {}

  // Ddk Apis.
  static zx_status_t Bind(void* ctx, zx_device_t* device);
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);
  void DdkRelease();

 private:
  // fuchsia.hardware.bluetooth.Hci APIs.
  void OpenCommandChannel(OpenCommandChannelRequestView request,
                          OpenCommandChannelCompleter::Sync& completer) override;
  void OpenAclDataChannel(OpenAclDataChannelRequestView request,
                          OpenAclDataChannelCompleter::Sync& completer) override;
  void OpenSnoopChannel(OpenSnoopChannelRequestView request,
                        OpenSnoopChannelCompleter::Sync& completer) override;
  bt_hci_protocol_t hci;
};

zx_status_t PassthroughDevice::DdkGetProtocol(uint32_t proto_id, void* out_proto) {
  if (proto_id != ZX_PROTOCOL_BT_HCI) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Forward the underlying bt-transport ops.
  bt_hci_protocol_t* hci_proto = static_cast<bt_hci_protocol_t*>(out_proto);
  hci_proto->ops = this->hci.ops;
  hci_proto->ctx = this->hci.ctx;
  return ZX_OK;
}

void PassthroughDevice::DdkRelease() { delete this; }

void PassthroughDevice::OpenCommandChannel(OpenCommandChannelRequestView request,
                                           OpenCommandChannelCompleter::Sync& completer) {
  if (zx_status_t status = bt_hci_open_command_channel(&this->hci, request->channel.release());
      status != ZX_OK) {
    completer.Close(status);
  }
}

void PassthroughDevice::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                           OpenAclDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = bt_hci_open_acl_data_channel(&this->hci, request->channel.release());
      status != ZX_OK) {
    completer.Close(status);
  }
}

void PassthroughDevice::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                         OpenSnoopChannelCompleter::Sync& completer) {
  if (zx_status_t status = bt_hci_open_snoop_channel(&this->hci, request->channel.release());
      status != ZX_OK) {
    completer.Close(status);
  }
}

zx_status_t PassthroughDevice::Bind(void* ctx, zx_device_t* device) {
  zxlogf(INFO, "bt_hci_passthrough_bind: starting");
  std::unique_ptr passthrough = std::make_unique<PassthroughDevice>(device);

  zx_status_t status = device_get_protocol(device, ZX_PROTOCOL_BT_HCI, &passthrough->hci);
  if (status != ZX_OK) {
    zxlogf(ERROR, "bt_hci_passthrough_bind: failed protocol: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = passthrough->DdkAdd(
          ddk::DeviceAddArgs("bt_hci_passthrough").set_proto_id(ZX_PROTOCOL_BT_HCI));
      status != ZX_OK) {
    zxlogf(ERROR, "bt_hci_passthrough_bind failed: %s", zx_status_get_string(status));
    return status;
  }

  // The driver framework now owns the memory for passthrough.
  std::ignore = passthrough.release();
  return ZX_OK;
}

zx_driver_ops_t bt_hci_passthrough_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = PassthroughDevice::Bind,
};

}  // namespace

// This should be the last driver queried, so we match any transport.
ZIRCON_DRIVER(bt_hci_passthrough, bt_hci_passthrough_driver_ops, "fuchsia", "0.1");
