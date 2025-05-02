// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_IMPL_H_
#define SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_IMPL_H_

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <lib/zx/channel.h>

#include "wlantap-driver-context.h"
#include "wlantap-mac.h"
#include "wlantap-phy.h"

namespace wlan {

// Serves the WlanPhyImpl protocol. This also creates an instance of WlantapPhy, which lets the test
// suite control the state of the mock driver.
class WlanPhyImplDevice : public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl>,
                          public std::enable_shared_from_this<WlanPhyImplDevice> {
  using NodeControllerClient = fidl::ClientEnd<fuchsia_driver_framework::NodeController>;

 public:
  WlanPhyImplDevice() = delete;

  // Allocates a WlanPhyImplDevice into a std::shared_ptr so that WlanPhyImplDevice
  // in its implementation can create additional references to the std::shared_ptr
  // for use by WlantapPhy and shutdown callbacks.
  static std::shared_ptr<WlanPhyImplDevice> New(
      const std::shared_ptr<const WlantapDriverContext>& context, zx::channel user_channel,
      const std::shared_ptr<const wlan_tap::WlantapPhyConfig>& phy_config,
      NodeControllerClient phy_controller);

  // WlanPhyImpl protocol implementation
  void GetSupportedMacRoles(fdf::Arena& arena,
                            GetSupportedMacRolesCompleter::Sync& completer) override;
  void CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                   CreateIfaceCompleter::Sync& completer) override;
  void DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                    DestroyIfaceCompleter::Sync& completer) override;
  void SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                  SetCountryCompleter::Sync& completer) override;
  void ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) override;
  void GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) override;
  void SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                        SetPowerSaveModeCompleter::Sync& completer) override;
  void GetPowerSaveMode(fdf::Arena& arena, GetPowerSaveModeCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_wlan_phyimpl::WlanPhyImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  WlanPhyImplDevice(const std::shared_ptr<const WlantapDriverContext>& context,
                    const std::shared_ptr<const wlan_tap::WlantapPhyConfig>& phy_config);
  void Init(zx::channel user_channel, NodeControllerClient phy_controller);

  zx_status_t CreateWlanSoftmac(wlan_common::WlanMacRole role,
                                zx::channel mlme_channel) __TA_NO_THREAD_SAFETY_ANALYSIS;
  zx_status_t AddWlanSoftmacChild(std::string_view name,
                                  fidl::ServerEnd<fuchsia_driver_framework::NodeController> server);
  zx_status_t ServeWlanSoftmac(std::string_view name, wlan_common::WlanMacRole role,
                               zx::channel mlme_channel);

  bool IfaceExists();
  fit::result<zx_status_t> DestroyIface();
  void ShutdownComplete();

  const std::shared_ptr<const WlantapDriverContext> driver_context_;

  const std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config_{};

  std::string name_{"wlanphyimpl"};

  // Initialize in Init() with a shared_ptr to this instance.
  std::unique_ptr<WlantapPhy> wlantap_phy_ = nullptr;

  std::unique_ptr<WlantapMac> wlantap_mac_;

  fidl::Client<fuchsia_driver_framework::NodeController> phy_controller_;

  fidl::Client<fuchsia_driver_framework::NodeController> iface_controller_;

  std::optional<WlantapPhy::ShutdownCompleter::Async> wlantap_phy_shutdown_completer_;
};

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_IMPL_H_
