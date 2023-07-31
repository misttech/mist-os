// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLANPHYIMPL_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLANPHYIMPL_DEVICE_H_

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/vector_view.h>

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/banjo/common.h"

struct iwl_mvm_vif;
struct iwl_trans;

namespace wlan::iwlwifi {

class WlanPhyImplDevice : public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl> {
 public:
  WlanPhyImplDevice(const WlanPhyImplDevice& device) = delete;
  WlanPhyImplDevice& operator=(const WlanPhyImplDevice& other) = delete;
  virtual ~WlanPhyImplDevice();
  explicit WlanPhyImplDevice();

  // Initialize the dispatcher which dipatcher which binds to the server end of
  // fuchsia_wlan_phyimpl::WlanphyImpl.
  zx_status_t InitServerDispatcher();

  // Implemented by driver class(PcieIwlwifiDriver).
  virtual zx_status_t AddWlansoftmacDevice(uint16_t iface_id, struct iwl_mvm_vif* mvmvif) = 0;
  virtual zx_status_t RemoveWlansoftmacDevice(uint16_t iface_id) = 0;

  // State accessors.
  virtual iwl_trans* drvdata() = 0;
  virtual const iwl_trans* drvdata() const = 0;

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

  void ServiceConnectHandler(fdf_dispatcher_t* dispatcher,
                             fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end);
};

}  // namespace wlan::iwlwifi

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLANPHYIMPL_DEVICE_H_
