// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/factory_device.h"

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/debug.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"

namespace wlan {
namespace brcmfmac {

FactoryDevice::FactoryDevice(zx_device_t* parent, struct brcmf_pub* brcmf_pub)
    : FactoryDeviceType(parent), brcmf_pub_(brcmf_pub) {}

zx_status_t FactoryDevice::Create(zx_device_t* parent_device, struct brcmf_pub* brcmf_pub,
                                  FactoryDevice** factory_device_) {
  zx_status_t status = ZX_OK;
  std::unique_ptr<FactoryDevice> device(new FactoryDevice(parent_device, brcmf_pub));

  if ((status = device->DdkAdd(
           ddk::DeviceAddArgs("brcmfmac-wlan-factory").set_proto_id(ZX_PROTOCOL_WLAN_FACTORY)))) {
    return status;
  }
  *factory_device_ = device.get();
  device.release();  // This now has its lifecycle managed by the fdf.

  return ZX_OK;
}

void FactoryDevice::Get(GetRequestView request, GetCompleter::Sync& _completer) {
  zx_status_t status =
      brcmf_send_cmd_to_firmware(brcmf_pub_, request->iface_idx, request->cmd,
                                 (void*)request->request.data(), request->request.count(), false);
  if (status == ZX_OK) {
    _completer.ReplySuccess(request->request);
  } else {
    _completer.ReplyError(status);
  }
}

void FactoryDevice::Set(SetRequestView request, SetCompleter::Sync& _completer) {
  zx_status_t status =
      brcmf_send_cmd_to_firmware(brcmf_pub_, request->iface_idx, request->cmd,
                                 (void*)request->request.data(), request->request.count(), true);
  if (status == ZX_OK) {
    _completer.ReplySuccess();
  } else {
    _completer.ReplyError(status);
  }
}

void FactoryDevice::DdkRelease() { delete this; }

}  // namespace brcmfmac
}  // namespace wlan
