// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_CTL_H_
#define SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_CTL_H_

#include <fidl/fuchsia.wlan.tap/cpp/fidl.h>

#include "wlantap-driver-context.h"
#include "wlantap-phy-impl.h"

namespace wlan {

class WlantapCtlServer : public fidl::WireServer<fuchsia_wlan_tap::WlantapCtl> {
 public:
  explicit WlantapCtlServer(const std::shared_ptr<const WlantapDriverContext>& context)
      : driver_context_(context), logger_(driver_context_->logger()) {}

  // WlantapCtl protocol implementation
  void CreatePhy(CreatePhyRequestView request, CreatePhyCompleter::Sync& completer) override;

 private:
  zx_status_t AddWlanPhyImplChild(std::string_view name,
                                  fidl::ServerEnd<fuchsia_driver_framework::NodeController> server);
  zx_status_t ServeWlanPhyImplProtocol(std::string_view name,
                                       std::shared_ptr<WlanPhyImplDevice> impl);

  static constexpr size_t kWlantapPhyConfigBufferSize =
      fidl::MaxSizeInChannel<fuchsia_wlan_tap::wire::WlantapPhyConfig,
                             fidl::MessageDirection::kSending>();

  const std::shared_ptr<const WlantapDriverContext> driver_context_;
  const fdf::Logger* logger_;
  fidl::Arena<kWlantapPhyConfigBufferSize> phy_config_arena_;
};

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_CTL_H_
