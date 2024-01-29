// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANPHY_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANPHY_DEVICE_H_

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.wlan.device/cpp/wire.h>
#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.phyimpl/cpp/wire.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/sync/cpp/completion.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace wlanphy {

class DeviceConnector;

class Device final : public fdf::DriverBase,
                     public fidl::WireServer<fuchsia_wlan_device::Phy>,
                     public fidl::WireServer<fuchsia_wlan_device::Connector>,
                     public fidl::WireAsyncEventHandler<fdf::NodeController> {
 public:
  explicit Device(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  static constexpr const char* Name() { return "wlanphy"; }
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Function implementations in protocol fuchsia_wlan_device::Phy.
  void GetSupportedMacRoles(GetSupportedMacRolesCompleter::Sync& completer) override;
  void CreateIface(CreateIfaceRequestView request, CreateIfaceCompleter::Sync& completer) override;
  void DestroyIface(DestroyIfaceRequestView request,
                    DestroyIfaceCompleter::Sync& completer) override;
  void SetCountry(SetCountryRequestView request, SetCountryCompleter::Sync& completer) override;
  void GetCountry(GetCountryCompleter::Sync& completer) override;
  void ClearCountry(ClearCountryCompleter::Sync& completer) override;
  void SetPowerSaveMode(SetPowerSaveModeRequestView request,
                        SetPowerSaveModeCompleter::Sync& completer) override;
  void GetPowerSaveMode(GetPowerSaveModeCompleter::Sync& completer) override;

  // Function implementations in protocol fuchsia_wlan_device::Connector.
  void Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) override;
  void ConnectPhyServerEnd(fidl::ServerEnd<fuchsia_wlan_device::Phy> server_end);

  zx_status_t ConnectToWlanPhyImpl();

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

 private:
  void Serve(fidl::ServerEnd<fuchsia_wlan_device::Connector> server) {
    bindings_.AddBinding(dispatcher(), std::move(server), this, fidl::kIgnoreBindingClosure);
  }
  zx_status_t AddWlanDeviceConnector();

  // The FIDL client to communicate with iwlwifi
  fdf::WireSharedClient<fuchsia_wlan_phyimpl::WlanPhyImpl> client_;

  // Dispatcher for being a FIDL client firing requests to WlanPhyImpl device.
  fdf::Dispatcher client_dispatcher_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  driver_devfs::Connector<fuchsia_wlan_device::Connector> devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_wlan_device::Connector> bindings_;
  fidl::ServerBindingGroup<fuchsia_wlan_device::Phy> phy_bindings_;

  friend class DeviceConnector;
};

}  // namespace wlanphy

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANPHY_DEVICE_H_
