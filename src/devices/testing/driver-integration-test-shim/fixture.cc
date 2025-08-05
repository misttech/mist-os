// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "include/lib/driver-integration-test/fixture.h"

#include <fidl/fuchsia.board.test/cpp/wire.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.sysinfo/cpp/wire_test_base.h>
#include <fidl/fuchsia.system.state/cpp/wire.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/vfs/cpp/service.h>

#include <bind/fuchsia/platform/cpp/bind.h>

#include "lib/sys/component/cpp/testing/realm_builder.h"

namespace driver_integration_test {

using namespace component_testing;

class FakeSysinfo : public LocalComponentImpl,
                    public fidl::testing::WireTestBase<fuchsia_sysinfo::SysInfo> {
 public:
  explicit FakeSysinfo(std::string board_name) : board_name_(std::move(board_name)) {}

  void OnStart() override {
    auto service =
        std::make_unique<vfs::Service>([this](zx::channel request, async_dispatcher_t* dispatcher) {
          bindings_.AddBinding(dispatcher,
                               fidl::ServerEnd<fuchsia_sysinfo::SysInfo>(std::move(request)), this,
                               fidl::kIgnoreBindingClosure);
        });
    ZX_ASSERT(outgoing()->AddPublicService(std::move(service), "fuchsia.sysinfo.SysInfo") == ZX_OK);
  }

  void GetBoardName(GetBoardNameCompleter::Sync& completer) override {
    completer.Reply(ZX_OK, fidl::StringView::FromExternal(board_name_));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ZX_PANIC("Unexpected call to sysinfo: %s", name.c_str());
  }

 private:
  fidl::ServerBindingGroup<fuchsia_sysinfo::SysInfo> bindings_;
  std::string board_name_;
};

class FakeBootArgsComponent : public LocalComponentImpl {
 public:
  explicit FakeBootArgsComponent(std::unique_ptr<fidl::WireServer<fuchsia_boot::Arguments>> server)
      : server_(std::move(server)) {}

  void OnStart() override {
    auto service = std::make_unique<vfs::Service>([this](zx::channel request,
                                                         async_dispatcher_t* dispatcher) {
      bindings_.AddBinding(dispatcher, fidl::ServerEnd<fuchsia_boot::Arguments>(std::move(request)),
                           server_.get(), fidl::kIgnoreBindingClosure);
    });
    ZX_ASSERT(outgoing()->AddPublicService(std::move(service), "fuchsia.boot.Arguments") == ZX_OK);
  }

 private:
  std::unique_ptr<fidl::WireServer<fuchsia_boot::Arguments>> server_;
  fidl::ServerBindingGroup<fuchsia_boot::Arguments> bindings_;
};

zx_status_t IsolatedDevmgr::Create(Args* args, IsolatedDevmgr* out) {
  IsolatedDevmgr devmgr;
  devmgr.loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread);
  devmgr.loop_->StartThread();

  // Create and build the realm.
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);

  // Setup Fshost.
  if (args->enable_storage_host) {
    realm_builder.AddChild("fshost", "#meta/test-fshost-storage-host.cm");
    realm_builder.AddChild("fshost_config", "#meta/test-fshost-storage-host_config.cm");
  } else {
    realm_builder.AddChild("fshost", "#meta/test-fshost.cm");
    realm_builder.AddChild("fshost_config", "#meta/test-fshost_config.cm");
    if (args->disable_block_watcher) {
      args->fshost_config.emplace_back(ConfigCapability{
          .name = "fuchsia.fshost.DisableBlockWatcher", .value = ConfigValue::Bool(true)});
    }
  }

  std::vector<Capability> config_routes = {
      Config{.name = "fuchsia.fshost.Blobfs"},
      Config{.name = "fuchsia.fshost.BlobfsInitialInodes"},
      Config{.name = "fuchsia.fshost.BlobfsMaxBytes"},
      Config{.name = "fuchsia.fshost.BlobfsUseDeprecatedPaddedFormat"},
      Config{.name = "fuchsia.fshost.BootPart"},
      Config{.name = "fuchsia.fshost.CheckFilesystems"},
      Config{.name = "fuchsia.fshost.Data"},
      Config{.name = "fuchsia.fshost.DataFilesystemFormat"},
      Config{.name = "fuchsia.fshost.DataMaxBytes"},
      Config{.name = "fuchsia.fshost.DisableBlockWatcher"},
      Config{.name = "fuchsia.fshost.Factory"},
      Config{.name = "fuchsia.fshost.FormatDataOnCorruption"},
      Config{.name = "fuchsia.fshost.Fvm"},
      Config{.name = "fuchsia.fshost.FvmSliceSize"},
      Config{.name = "fuchsia.fshost.FxfsBlob"},
      Config{.name = "fuchsia.fshost.Gpt"},
      Config{.name = "fuchsia.fshost.GptAll"},
      Config{.name = "fuchsia.fshost.Mbr"},
      Config{.name = "fuchsia.fshost.Nand"},
      Config{.name = "fuchsia.fshost.NoZxcrypt"},
      Config{.name = "fuchsia.fshost.RamdiskImage"},
      Config{.name = "fuchsia.fshost.StorageHost"},
      Config{.name = "fuchsia.fshost.StorageHostUrl"},
      Config{.name = "fuchsia.fshost.UseDiskMigration"},
      Config{.name = "fuchsia.fshost.FxfsCryptUrl"},
      Config{.name = "fuchsia.fshost.StarnixVolumeName"},
      Config{.name = "fuchsia.fshost.InlineCrypto"},
      Config{.name = "fuchsia.fshost.ProvisionFxfs"},
  };

  if (!args->fshost_config.empty()) {
    std::vector<Capability> config_self_routes;
    for (const auto& config : args->fshost_config) {
      std::erase_if(config_routes, [&config](const auto& route) {
        const auto* config_route = std::get_if<Config>(&route);
        return config_route && config_route->name == config.name;
      });
      config_self_routes.emplace_back(Config{.name = config.name});
    }
    realm_builder.AddRoute(Route{
        .capabilities = config_self_routes,
        .source = {SelfRef{}},
        .targets = {ChildRef{"fshost"}},
    });
    realm_builder.AddConfiguration(std::move(args->fshost_config));
  }

  realm_builder.AddRoute(Route{
      .capabilities = config_routes,
      .source = {ChildRef{"fshost_config"}},
      .targets = {ChildRef{"fshost"}},
  });

  realm_builder.AddRoute(Route{
      .capabilities =
          {
              Config{.name = "fuchsia.fshost.DisableAutomount"},
              Config{.name = "fuchsia.blobfs.WriteCompressionAlgorithm"},
              Config{.name = "fuchsia.blobfs.CacheEvictionPolicy"},
          },
      .source = {VoidRef()},
      .targets = {ChildRef{"fshost"}},
  });

  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.process.Launcher"}},
      .source = {ParentRef()},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.system.state.Administrator"}},
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
      .capabilities =
          {
              Protocol{"fuchsia.fshost.Admin"},
              Protocol{"fuchsia.fshost.Recovery"},
              Protocol{"fuchsia.storage.partitions.PartitionsManager"},
              Service{"fuchsia.storage.partitions.PartitionService"},
          },
      .source = {ChildRef{"fshost"}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "block", .rights = fuchsia::io::R_STAR_DIR}},
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
  realm_builder.AddRoute(Route{
      .capabilities = {Service{"fuchsia.hardware.block.volume.Service"}},
      .source = {ChildRef{"driver_test_realm"}},
      .targets = {ChildRef{"fshost"}},
  });
  realm_builder.AddLocalChild("fake-sysinfo",
                              [board_name = std::string(args->board_name)]() mutable {
                                return std::make_unique<FakeSysinfo>(board_name);
                              });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.sysinfo.SysInfo"}},
      .source = {ChildRef{"fake-sysinfo"}},
      .targets = {ParentRef()},
  });
  if (args->fake_boot_args) {
    realm_builder.AddLocalChild("fake-bootargs",
                                [impl = std::move(args->fake_boot_args)]() mutable {
                                  return std::make_unique<FakeBootArgsComponent>(std::move(impl));
                                });
    realm_builder.AddRoute(Route{
        .capabilities = {Protocol{"fuchsia.boot.Arguments"}},
        .source = {ChildRef{"fake-bootargs"}},
        .targets = {ParentRef()},
    });
  }

  std::vector<fuchsia_component_test::Capability> exposes = {{
      fuchsia_component_test::Capability::WithService(
          fuchsia_component_test::Service{{.name = "fuchsia.hardware.block.volume.Service"}}),
      fuchsia_component_test::Capability::WithService(
          fuchsia_component_test::Service{{.name = "fuchsia.hardware.skipblock.Service"}}),
      fuchsia_component_test::Capability::WithService(
          fuchsia_component_test::Service{{.name = "fuchsia.hardware.ramdisk.Service"}}),
      fuchsia_component_test::Capability::WithService(
          fuchsia_component_test::Service{{.name = "fuchsia.storage.ftl.Service"}}),
  }};
  driver_test_realm::AddDtrExposes(realm_builder, exposes);

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
  realm_args.set_root_driver("fuchsia-boot:///platform-bus#meta/platform-bus.cm");
  realm_args.set_driver_log_level(args->log_level);
  realm_args.set_board_name(std::string(args->board_name.data()));
  realm_args.set_driver_disable(args->driver_disable);
  realm_args.set_driver_bind_eager(args->driver_bind_eager);
  realm_args.set_software_devices(std::vector{
      fuchsia::driver::test::SoftwareDevice{
          .device_name = "ram-disk",
          .device_id = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_RAM_DISK,
      },
      fuchsia::driver::test::SoftwareDevice{
          .device_name = "ram-nand",
          .device_id = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_RAM_NAND,
      },
  });
  realm_args.mutable_exposes()->emplace_back(
      fuchsia::driver::test::Expose{.service_name = "fuchsia.storage.ftl.Service",
                                    .collection = fuchsia::driver::test::Collection::BOOT_DRIVERS});
  realm_args.mutable_exposes()->emplace_back(
      fuchsia::driver::test::Expose{.service_name = "fuchsia.hardware.skipblock.Service",
                                    .collection = fuchsia::driver::test::Collection::BOOT_DRIVERS});
  if (zx_status_t status = driver_test_realm->Start(std::move(realm_args), &realm_result);
      status != ZX_OK) {
    return status;
  }
  if (realm_result.is_err()) {
    return realm_result.err();
  }

  // Connect to dev.
  fidl::InterfaceHandle<fuchsia::io::Node> dev;
  if (zx_status_t status = devmgr.realm_->component().exposed()->Open(
          "dev-topological", fuchsia::io::PERM_READABLE, {}, dev.NewRequest().TakeChannel());
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

  // Connect to fshost to ensure it starts up and watches for block devices.
  if (zx::result result = devmgr.realm_->component().Connect<fuchsia_fshost::Admin>();
      result.is_error()) {
    return result.status_value();
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

}  // namespace driver_integration_test
