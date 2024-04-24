// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_TEST_STUB_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_TEST_STUB_DEVICE_H_

#include <zircon/types.h>

#include <memory>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/device.h"

struct async_dispatcher;

namespace wlan {
namespace brcmfmac {

class DeviceInspect;

// A stub implementatkon of Device.
class StubDevice : public Device {
 public:
  StubDevice();
  ~StubDevice() override;

  zx_status_t BusInit() override { return ZX_OK; }
  // Device implementation.
  async_dispatcher_t* GetTimerDispatcher() override { return nullptr; }
  DeviceInspect* GetInspect() override { return nullptr; }
  zx_status_t LoadFirmware(const char* path, zx_handle_t* fw, size_t* size) override;
  zx_status_t DeviceGetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) override;
  fidl::WireClient<fdf::Node>& GetParentNode() override { return parent_node_; }
  std::shared_ptr<fdf::OutgoingDirectory>& Outgoing() override { return outgoing_dir_.value(); }
  const std::shared_ptr<fdf::Namespace>& Incoming() const override { return incoming_dir_.value(); }
  fdf_dispatcher_t* GetDriverDispatcher() override { return nullptr; }

 protected:
  fidl::WireClient<fdf::Node> parent_node_;
  std::optional<std::shared_ptr<fdf::OutgoingDirectory>> outgoing_dir_;
  std::optional<std::shared_ptr<fdf::Namespace>> incoming_dir_;
};

}  // namespace brcmfmac
}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_TEST_STUB_DEVICE_H_
