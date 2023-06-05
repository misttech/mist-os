// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_IMPL_H_
#define SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_IMPL_H_

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <lib/zx/channel.h>

#include "wlantap-driver-context.h"
#include "wlantap-phy.h"

namespace wlan {

// Serves the WlanPhyImpl protocol. This also creates an instance of WlantapPhy, which lets the test
// suite control the state of the mock driver.
class WlanPhyImplDevice : public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl> {
  using NodeControllerClient = fidl::ClientEnd<fuchsia_driver_framework::NodeController>;

 public:
  WlanPhyImplDevice(WlantapDriverContext context, zx::channel user_channel,
                    std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config,
                    NodeControllerClient phy_controller);

  ~WlanPhyImplDevice() override = default;

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

 private:
  zx_status_t CreateWlanSoftmac(wlan_common::WlanMacRole role, zx::channel mlme_channel);
  zx_status_t AddWlanSoftmacChild(std::string_view name,
                                  fidl::ServerEnd<fuchsia_driver_framework::NodeController> server);
  zx_status_t ServeWlanSoftmac(std::string_view name, std::unique_ptr<WlantapMac> impl);

  WlantapDriverContext driver_context_;

  fdf::Logger* logger_{nullptr};

  std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config_{};

  std::string name_{"wlanphyimpl"};

  std::unique_ptr<WlantapPhy> wlantap_phy_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> softmac_controller_;
};

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_IMPL_H_
