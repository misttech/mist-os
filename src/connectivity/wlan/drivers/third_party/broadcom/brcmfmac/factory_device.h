// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_FACTORY_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_FACTORY_DEVICE_H_

#include <fidl/fuchsia.factory.wlan/cpp/wire.h>

#include <ddktl/device.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/core.h"

namespace wlan {
namespace brcmfmac {

class FactoryDevice;
using FactoryDeviceType =
    ::ddk::Device<FactoryDevice, ddk::Messageable<fuchsia_factory_wlan::Iovar>::Mixin>;
class FactoryDevice : public FactoryDeviceType {
  FactoryDevice(zx_device_t* parent, struct brcmf_pub* brcmf_pub);

 public:
  void DdkRelease();

  // Static factory function for SdioDevice instances. This factory does not return an owned
  // instance, as on successful invocation the instance will have its lifecycle managed by the
  // devhost.
  static zx_status_t Create(zx_device_t* parent_device, struct brcmf_pub* brcmf_pub,
                            FactoryDevice** factory_device);

 private:
  // fidl::WireServer<fuchsia_factory_wlan_iovar::Iovar> Implementation
  void Get(GetRequestView request, GetCompleter::Sync& _completer) override;
  void Set(SetRequestView request, SetCompleter::Sync& _completer) override;

  brcmf_pub* brcmf_pub_;
};

}  // namespace brcmfmac
}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_FACTORY_DEVICE_H_
