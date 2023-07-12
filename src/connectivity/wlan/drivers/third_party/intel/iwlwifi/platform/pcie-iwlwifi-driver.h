// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_PCIE_IWLWIFI_DRIVER_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_PCIE_IWLWIFI_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/sync/cpp/completion.h>

#include <memory>

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/kernel.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/wlanphyimpl-device.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/wlansoftmac-device.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace wlan::iwlwifi {

class DriverInspector;
class RcuManager;

// This class contains the Fuchsia-specific PCIE bus initialization logic, and uses DDKTL class
// to manage the lifetime of a iwlwifi driver instance.
class PcieIwlwifiDriver : public wlan::iwlwifi::WlanPhyImplDevice,
                          public fdf::DriverBase,
                          public fidl::WireAsyncEventHandler<fdf::NodeController> {
 public:
  PcieIwlwifiDriver(const PcieIwlwifiDriver& driver) = delete;
  PcieIwlwifiDriver& operator=(const PcieIwlwifiDriver& driver_in) = delete;
  explicit PcieIwlwifiDriver(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~PcieIwlwifiDriver();

  static constexpr const char* Name() { return "iwlwifi"; }
  // The start point of iwlwifi driver. This function will be called right after the driver
  // framework decides to bind this driver to a known node.
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Device implementation.
  iwl_trans* drvdata() override;
  const iwl_trans* drvdata() const override;

  // Add a child node to represent WlanPhyImplDevice in DFv2.
  zx_status_t AddWlanphyChild();

  // Create a WlanSoftmacDevice and also add a child node to represent it in DFv2.
  zx_status_t AddWlansoftmacDevice(uint16_t iface_id, struct iwl_mvm_vif* mvmvif) override;

  zx_status_t RemoveWlansoftmacDevice(uint16_t iface_id) override;

  // It is the handler that will be register for protocol fuchsia_wlan_wlansoftmac::WlanSoftmac.
  // It'll be called when downstream driver trys to establish a connection based on this protocol
  // with this driver.
  zx_status_t WlanSoftmacConnectHandler(fdf::Channel channel);

  // The callback called by module.cc when loading firmware, this function returns firmware file vmo
  // and size.
  zx_status_t LoadFirmware(const char* name, zx_handle_t* vmo, size_t* size);

  // Overriding on_fidl_error from WireAsyncEventHandler.
  void on_fidl_error(fidl::UnbindInfo error) override {
    ZX_ASSERT(error.status() == ZX_ERR_PEER_CLOSED);
    if (!wlan_softmac_device_) {
      if (prepare_stop_completer_)
        (*prepare_stop_completer_)(zx::ok());
      return;
    }

    wlan_softmac_device_.reset();
    auto result = wlansoftmac_controller_client_->Remove();
    ZX_ASSERT_MSG(result.ok(), "Softmac child remove failed, FIDL error: %s",
                  result.status_string());
  }

 private:
  zx_status_t Init();

  zx_status_t StartPci();

  // Adding WlanPhyImpl service to outgoing directory.
  zx_status_t AddWlanPhyImplService();

  std::unique_ptr<DriverInspector> driver_inspector_;
  std::unique_ptr<RcuManager> rcu_manager_;

  // FIDL client of the node that this driver binds to.
  fidl::WireClient<fdf::Node> node_client_;
  fidl::WireClient<fdf::NodeController> wlanphy_controller_client_;

  // Iwlwifi only supports client iface now.
  fidl::WireClient<fdf::NodeController> wlansoftmac_controller_client_;
  std::unique_ptr<WlanSoftmacDevice> wlan_softmac_device_;

  std::optional<fdf::PrepareStopCompleter> prepare_stop_completer_;

  iwl_pci_dev pci_dev_;
};

}  // namespace wlan::iwlwifi

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_PCIE_IWLWIFI_DRIVER_H_
