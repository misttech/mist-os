// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "include/lib/driver-integration-test/fixture.h"

#include <fidl/fuchsia.board.test/cpp/wire.h>
#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/syslog/global.h>

#include "lib/sys/component/cpp/testing/realm_builder.h"

namespace driver_integration_test {

using namespace component_testing;

zx_status_t IsolatedDevmgr::Create(Args* args, IsolatedDevmgr* out) {
  IsolatedDevmgr devmgr;
  devmgr.loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread);

  // Create and build the realm.
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);

  // Setup Fshost.
  if (args->disable_block_watcher) {
    realm_builder.AddChild("fshost", "#meta/test-fshost-no-watcher.cm");
  } else {
    realm_builder.AddChild("fshost", "#meta/test-fshost.cm");
  }
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.process.Launcher"}},
      .source = {ParentRef()},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.device.manager.Administrator"}},
      .source = {ChildRef{"driver_test_realm"}},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.hardware.power.statecontrol.Admin"}},
      .source = {ChildRef{"driver_test_realm"}},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.logger.LogSink"}},
      .source = {ParentRef()},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.fshost.BlockWatcher"}, Protocol{"fuchsia.fshost.Admin"}},
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "factory", .rights = fuchsia::io::R_STAR_DIR}},
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "durable", .rights = fuchsia::io::RW_STAR_DIR}},
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "install", .rights = fuchsia::io::RW_STAR_DIR}},
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "tmp", .rights = fuchsia::io::RW_STAR_DIR}},
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "volume", .rights = fuchsia::io::RW_STAR_DIR}},
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });

  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "dev-topological", .rights = fuchsia::io::R_STAR_DIR}},
      .source = {ChildRef{"driver_test_realm"}},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "dev-class", .rights = fuchsia::io::R_STAR_DIR}},
      .source = {ChildRef{"driver_test_realm"}},
      .targets = {ChildRef{"fshost"}},
  });

  // Build the realm.
  devmgr.realm_ = std::make_unique<component_testing::RealmRoot>(
      realm_builder.Build(devmgr.loop_->dispatcher()));

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  if (zx_status_t status = devmgr.realm_->component().Connect(driver_test_realm.NewRequest());
      status != ZX_OK) {
    return status;
  }

  fuchsia::driver::test::Realm_Start_Result realm_result;
  auto realm_args = fuchsia::driver::test::RealmArgs();
  realm_args.set_use_driver_framework_v2(args->use_driver_framework_v2);
  realm_args.set_root_driver("fuchsia-boot:///platform-bus#meta/platform-bus.cm");
  realm_args.set_driver_log_level(args->log_level);
  realm_args.set_board_name(std::string(args->board_name.data()));
  realm_args.set_driver_disable(args->driver_disable);
  realm_args.set_driver_bind_eager(args->driver_bind_eager);
  if (zx_status_t status = driver_test_realm->Start(std::move(realm_args), &realm_result);
      status != ZX_OK) {
    return status;
  }
  if (realm_result.is_err()) {
    return realm_result.err();
  }

  // Connect to dev.
  fidl::InterfaceHandle<fuchsia::io::Node> dev;
  if (zx_status_t status =
          devmgr.realm_->component().Connect("dev-topological", dev.NewRequest().TakeChannel());
      status != ZX_OK) {
    return status;
  }

  if (zx_status_t status =
          fdio_fd_create(dev.TakeChannel().release(), devmgr.devfs_root_.reset_and_get_address());
      status != ZX_OK) {
    return status;
  }

  zx::result channel =
      device_watcher::RecursiveWaitForFile(devmgr.devfs_root_.get(), "sys/platform/pt/test-board");
  if (channel.is_error()) {
    return channel.status_value();
  }

  fidl::ClientEnd<fuchsia_board_test::Board> client_end(std::move(channel.value()));
  fidl::WireSyncClient client(std::move(client_end));

  for (auto& device : args->device_list) {
    std::vector<uint8_t> metadata(device.metadata, device.metadata + device.metadata_size);
    const fidl::WireResult result = client->CreateDevice({
        .name = fidl::StringView::FromExternal(device.name),
        .metadata = fidl::VectorView<uint8_t>::FromExternal(metadata),
        .vid = device.vid,
        .pid = device.pid,
        .did = device.did,
    });
    if (!result.ok()) {
      return result.status();
    }
  }

  *out = std::move(devmgr);
  return ZX_OK;
}

zx_status_t IsolatedDevmgr::SuspendDriverManager() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_device_manager::Administrator>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }
  zx_status_t status = realm_->component().Connect(
      fidl::DiscoverableProtocolName<fuchsia_device_manager::Administrator>,
      endpoints->server.TakeChannel());
  if (status != ZX_OK) {
    return status;
  }
  auto result = fidl::WireCall(endpoints->client)->SuspendWithoutExit();
  if (!result.ok()) {
    return result.status();
  }

  return ZX_OK;
}

}  // namespace driver_integration_test
