// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "wlantap-ctl.h"
#include "wlantap-driver-context.h"

namespace wlan {

// The actual driver class. This main responsibility of this class is to serve the WlantapCtl
// protocol over devfs so that it's discoverable by wlandevicemonitor. It also passes on a
// WlantapDriverContext to the spawned WlantapCtl instance so that WlantapCtl can add child nodes
// and serve new protocols.
class WlantapDriver : public fdf::DriverBase {
  static constexpr std::string_view kDriverName = "wlantapctl";

 public:
  WlantapDriver(fdf::DriverStartArgs start_args,
                fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&WlantapDriver::Serve>(this)) {}

  zx::result<> Start() override {
    node_.Bind(std::move(node()));
    fidl::Arena arena;

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    // By calling AddChild with devfs_args, the child driver will be discoverable through devfs.
    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, kDriverName)
                    .devfs_args(devfs.Build())
                    .Build();

    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT(controller_endpoints.is_ok());

    auto result = node_->AddChild(args, std::move(controller_endpoints->server), {});
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
      return zx::error(result.status());
    }

    return zx::ok();
  }

 private:
  void Serve(fidl::ServerEnd<fuchsia_wlan_tap::WlantapCtl> server) {
    // Give the dispatcher ownership of server_impl
    auto server_impl =
        std::make_unique<WlantapCtlServer>(WlantapDriverContext(&logger(), outgoing(), &node_));
    fidl::BindServer(dispatcher(), std::move(server), std::move(server_impl));
  }

  // The node client. This lets WlantapDriver and related classes add child nodes, which is the DFv2
  // equivalent of calling device_add().
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;

  // devfs_connector_ lets the class serve the WlantapCtl protocol over devfs.
  driver_devfs::Connector<fuchsia_wlan_tap::WlantapCtl> devfs_connector_;
};

}  // namespace wlan
FUCHSIA_DRIVER_LIFECYCLE_CPP_V2(fdf::Lifecycle<wlan::WlantapDriver>);
