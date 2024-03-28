// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component.resolution/cpp/wire.h>
#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <fidl/fuchsia.pkg/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fdio/directory.h>
#include <lib/stdcompat/string_view.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/syslog/global.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/job.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>

#include <memory>
#include <unordered_map>
#include <vector>

#include <ddk/metadata/test.h>
#include <fbl/string_printf.h>
#include <fbl/unique_fd.h>

#include "sdk/lib/driver_test_realm/driver_test_realm_config.h"
#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/concatenate.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/fxl/strings/substitute.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/pseudo_file.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

namespace {

namespace fio = fuchsia_io;
namespace fdt = fuchsia_driver_test;

using namespace component_testing;

// This board driver knows how to interpret the metadata for which devices to
// spawn.
const zbi_platform_id_t kPlatformId = []() {
  zbi_platform_id_t plat_id = {};
  plat_id.vid = PDEV_VID_TEST;
  plat_id.pid = PDEV_PID_PBUS_TEST;
  strcpy(plat_id.board_name, "driver-integration-test");
  return plat_id;
}();

#define BOARD_REVISION_TEST 42

const zbi_board_info_t kBoardInfo = []() {
  zbi_board_info_t board_info = {};
  board_info.revision = BOARD_REVISION_TEST;
  return board_info;
}();

// This function is responsible for serializing driver data. It must be kept
// updated with the function that deserialized the data. This function
// is TestBoard::FetchAndDeserialize.
zx_status_t GetBootItem(const std::vector<board_test::DeviceEntry>& entries, uint32_t type,
                        std::string_view board_name, uint32_t extra, zx::vmo* out,
                        uint32_t* length) {
  zx::vmo vmo;
  switch (type) {
    case ZBI_TYPE_PLATFORM_ID: {
      zbi_platform_id_t platform_id = kPlatformId;
      if (!board_name.empty()) {
        strncpy(platform_id.board_name, board_name.data(), ZBI_BOARD_NAME_LEN - 1);
      }
      zx_status_t status = zx::vmo::create(sizeof(kPlatformId), 0, &vmo);
      if (status != ZX_OK) {
        return status;
      }
      status = vmo.write(&platform_id, 0, sizeof(kPlatformId));
      if (status != ZX_OK) {
        return status;
      }
      *length = sizeof(kPlatformId);
      break;
    }
    case ZBI_TYPE_DRV_BOARD_INFO: {
      zx_status_t status = zx::vmo::create(sizeof(kBoardInfo), 0, &vmo);
      if (status != ZX_OK) {
        return status;
      }
      status = vmo.write(&kBoardInfo, 0, sizeof(kBoardInfo));
      if (status != ZX_OK) {
        return status;
      }
      *length = sizeof(kBoardInfo);
      break;
    }
    case ZBI_TYPE_DRV_BOARD_PRIVATE: {
      size_t list_size = sizeof(board_test::DeviceList);
      size_t entry_size = entries.size() * sizeof(board_test::DeviceEntry);

      size_t metadata_size = 0;
      for (const board_test::DeviceEntry& entry : entries) {
        metadata_size += entry.metadata_size;
      }

      zx_status_t status = zx::vmo::create(list_size + entry_size + metadata_size, 0, &vmo);
      if (status != ZX_OK) {
        return status;
      }

      // Write DeviceList to vmo.
      board_test::DeviceList list{.count = entries.size()};
      status = vmo.write(&list, 0, sizeof(list));
      if (status != ZX_OK) {
        return status;
      }

      // Write DeviceEntries to vmo.
      status = vmo.write(entries.data(), list_size, entry_size);
      if (status != ZX_OK) {
        return status;
      }

      // Write Metadata to vmo.
      size_t write_offset = list_size + entry_size;
      for (const board_test::DeviceEntry& entry : entries) {
        status = vmo.write(entry.metadata, write_offset, entry.metadata_size);
        if (status != ZX_OK) {
          return status;
        }
        write_offset += entry.metadata_size;
      }

      *length = static_cast<uint32_t>(list_size + entry_size + metadata_size);
      break;
    }
    default:
      break;
  }
  *out = std::move(vmo);
  return ZX_OK;
}

class FakeBootItems final : public fidl::WireServer<fuchsia_boot::Items> {
 public:
  void Get(GetRequestView request, GetCompleter::Sync& completer) override {
    zx::vmo vmo;
    uint32_t length = 0;
    std::vector<board_test::DeviceEntry> entries = {};
    zx_status_t status =
        GetBootItem(entries, request->type, board_name_, request->extra, &vmo, &length);
    if (status != ZX_OK) {
      FX_SLOG(ERROR, "Failed to get boot items", FX_KV("status", status));
    }
    completer.Reply(std::move(vmo), length);
  }
  void Get2(Get2RequestView request, Get2Completer::Sync& completer) override {
    FX_SLOG(ERROR, "Unsupported Get2 called.");
    completer.Close(ZX_OK);
  }

  void GetBootloaderFile(GetBootloaderFileRequestView request,
                         GetBootloaderFileCompleter::Sync& completer) override {
    completer.Reply(zx::vmo());
  }

  std::string board_name_;
};

class FakeSystemStateTransition final
    : public fidl::WireServer<fuchsia_device_manager::SystemStateTransition> {
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override {
    completer.Reply(fuchsia_device_manager::SystemPowerState::kFullyOn);
  }
  void GetMexecZbis(GetMexecZbisCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
};

class FakeRootJob final : public fidl::WireServer<fuchsia_kernel::RootJob> {
  void Get(GetCompleter::Sync& completer) override {
    zx::job job;
    zx_status_t status = zx::job::default_job()->duplicate(ZX_RIGHT_SAME_RIGHTS, &job);
    if (status != ZX_OK) {
      FX_SLOG(ERROR, "Failed to duplicate job", FX_KV("status", status));
    }
    completer.Reply(std::move(job));
  }
};

zx::result<fidl::ClientEnd<fio::Directory>> OpenPkgDir() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  zx_status_t status =
      fdio_open("/pkg",
                static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory |
                                      fuchsia_io::wire::OpenFlags::kRightReadable |
                                      fuchsia_io::wire::OpenFlags::kRightExecutable),
                endpoints->server.TakeChannel().release());
  if (status != ZX_OK) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(std::move(endpoints->client));
}

class DriverTestRealm final : public fidl::Server<fuchsia_driver_test::Realm> {
 public:
  DriverTestRealm(component::OutgoingDirectory* outgoing, async_dispatcher_t* dispatcher,
                  driver_test_realm_config::Config config)
      : outgoing_(outgoing), dispatcher_(dispatcher), config_(config) {}

  zx::result<> Init() {
    // We must connect capabilities up early as not all users wait for Start to complete before
    // trying to access the capabilities. The lack of synchronization with simple variants of DTR
    // in particular causes issues.
    for (auto& [dir, _, server_end] : directories_) {
      zx::result client_end = fidl::CreateEndpoints(&server_end);
      if (client_end.is_error()) {
        return client_end.take_error();
      }
      zx::result result = outgoing_->AddDirectory(std::move(client_end.value()), dir);
      if (result.is_error()) {
        FX_SLOG(ERROR, "Failed to add directory to outgoing directory", FX_KV("directory", dir));
        return result.take_error();
      }
    }

    const std::array<std::string, 3> kProtocols = {
        "fuchsia.device.manager.Administrator",
        "fuchsia.driver.development.Manager",
        "fuchsia.driver.registrar.DriverRegistrar",
    };
    for (const auto& protocol : kProtocols) {
      auto result = outgoing_->AddUnmanagedProtocol(
          [this, protocol](zx::channel request) {
            if (exposed_dir_.channel().is_valid()) {
              fdio_service_connect_at(exposed_dir_.channel().get(), protocol.c_str(),
                                      request.release());
            } else {
              // Queue these up to run later.
              cb_queue_.push_back([this, protocol, request = std::move(request)]() mutable {
                fdio_service_connect_at(exposed_dir_.channel().get(), protocol.c_str(),
                                        request.release());
              });
            }
          },
          protocol);
      if (result.is_error()) {
        FX_SLOG(ERROR, "Failed to add protocol to outgoing directory",
                FX_KV("protocol", protocol.c_str()));
        return result.take_error();
      }
    }

    // Hook up fuchsia.driver.test/Realm so we can proceed with the rest of initialization once
    // |Start| is invoked
    zx::result result = outgoing_->AddUnmanagedProtocol<fuchsia_driver_test::Realm>(
        bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
    if (result.is_error()) {
      FX_SLOG(ERROR, "Failed to add protocol to outgoing directory",
              FX_KV("protocol", "fuchsia.driver.test/Realm"));
      return result.take_error();
    }

    return zx::ok();
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    // Non-hermetic users will end up calling start several times as the component test framework
    // invokes the binary multiple times, resulting in main running several times. We may be
    // ignoring real issues by ignoreing the subsequent calls in the case that multiple parties
    // are invoking start unknowingly. Comparing the args may be a way to avoid that issue.
    // TODO(https://fxbug.dev/42073125): Remedy this situation
    if (is_started_) {
      completer.Reply(zx::ok());
      return;
    }
    is_started_ = true;

    // Tunnel fuchsia_boot::Items from parent to realm builder if |tunnel_boot_items| configuration
    // is set. If not, provide fuchsia_boot::Items from local.
    if (config_.tunnel_boot_items()) {
      zx::result result = outgoing_->AddUnmanagedProtocol<fuchsia_boot::Items>(
          [](fidl::ServerEnd<fuchsia_boot::Items> server_end) {
            if (const zx::result status = component::Connect<fuchsia_boot::Items>(
                    std::move(server_end),
                    fidl::DiscoverableProtocolDefaultPath<fuchsia_boot::Items>);
                status.is_error()) {
              FX_LOGS(ERROR) << "Failed to connect to fuchsia_boot::Items"
                             << status.status_string();
            }
          });
      if (result.is_error()) {
        completer.Reply(result.take_error());
        return;
      }
    } else {
      auto boot_items = std::make_unique<FakeBootItems>();
      if (request.args().board_name().has_value()) {
        boot_items->board_name_ = *request.args().board_name();
      }

      zx::result result = outgoing_->AddProtocol<fuchsia_boot::Items>(std::move(boot_items));
      if (result.is_error()) {
        completer.Reply(result.take_error());
        return;
      }
    }

    zx::result result = outgoing_->AddProtocol<fuchsia_device_manager::SystemStateTransition>(
        std::make_unique<FakeSystemStateTransition>());
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    result = outgoing_->AddProtocol<fuchsia_kernel::RootJob>(std::make_unique<FakeRootJob>());
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    // Setup /boot
    fidl::ClientEnd<fuchsia_io::Directory> boot_dir;
    if (request.args().boot().has_value()) {
      boot_dir = fidl::ClientEnd<fuchsia_io::Directory>(std::move(*request.args().boot()));
    } else {
      auto res = OpenPkgDir();
      if (res.is_error()) {
        completer.Reply(res.take_error());
        return;
      }
      boot_dir = std::move(res.value());
    }

    // Setup /pkg_drivers
    fidl::ClientEnd<fuchsia_io::Directory> pkg_drivers_dir;
    if (request.args().pkg().has_value()) {
      pkg_drivers_dir = fidl::ClientEnd<fuchsia_io::Directory>(std::move(*request.args().pkg()));
    } else {
      auto res = OpenPkgDir();
      if (res.is_error()) {
        completer.Reply(res.take_error());
        return;
      }
      pkg_drivers_dir = std::move(res.value());
    }

    // We only index /pkg if it's not identical to /boot.
    const bool create_pkg_config = request.args().pkg() || request.args().boot();

    zx::result base_and_boot_configs = ConstructBootAndBaseConfig(
        boot_dir,
        create_pkg_config ? pkg_drivers_dir : fidl::UnownedClientEnd<fuchsia_io::Directory>({}));
    if (base_and_boot_configs.is_error()) {
      completer.Reply(base_and_boot_configs.take_error());
      return;
    }

    result = outgoing_->AddDirectory(std::move(boot_dir), "boot");
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    result = outgoing_->AddDirectory(std::move(pkg_drivers_dir), "pkg_drivers");
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    // Add additional routes if specified.
    std::unordered_map<fdt::Collection, std::vector<Ref>> kMap = {
        {
            fdt::Collection::kBootDrivers,
            {
                CollectionRef{"boot-drivers"},
            },
        },
        {
            fdt::Collection::kPackageDrivers,
            {
                CollectionRef{"pkg-drivers"},
                CollectionRef{"full-pkg-drivers"},
            },
        },
    };
    if (request.args().offers().has_value()) {
      for (const auto& offer : *request.args().offers()) {
        realm_builder_.AddRoute(Route{.capabilities = {Protocol{offer.protocol_name()}},
                                      .source = {ParentRef()},
                                      .targets = kMap[offer.collection()]});
      }
    }

    if (request.args().exposes().has_value()) {
      for (const auto& expose : *request.args().exposes()) {
        for (const auto& ref : kMap[expose.collection()]) {
          realm_builder_.AddRoute(Route{.capabilities = {Service{expose.service_name()}},
                                        .source = ref,
                                        .targets = {ParentRef()}});
        }
      }
    }

    // Set driver-index config based on request.
    const std::vector<std::string> kEmptyVec;
    std::vector<component_testing::ConfigCapability> configurations;
    configurations.push_back({
        .name = "fuchsia.driver.BootDrivers",
        .value = std::move(base_and_boot_configs->boot_drivers),
    });
    configurations.push_back({
        .name = "fuchsia.driver.BaseDrivers",
        .value = std::move(base_and_boot_configs->base_drivers),
    });
    configurations.push_back({
        .name = "fuchsia.driver.BindEager",
        .value = request.args().driver_bind_eager().value_or(kEmptyVec),
    });
    configurations.push_back({
        .name = "fuchsia.driver.DisabledDrivers",
        .value = request.args().driver_disable().value_or(kEmptyVec),
    });
    realm_builder_.AddConfiguration(std::move(configurations));
    realm_builder_.AddRoute({
        .capabilities =
            {
                component_testing::Config{.name = "fuchsia.driver.BootDrivers"},
                component_testing::Config{.name = "fuchsia.driver.BaseDrivers"},
                component_testing::Config{.name = "fuchsia.driver.BindEager"},
                component_testing::Config{.name = "fuchsia.driver.DisabledDrivers"},
            },
        .source = component_testing::SelfRef{},
        .targets = {component_testing::ChildRef{"driver-index"}},
    });

    // Set driver_manager config based on request.
    configurations = std::vector<component_testing::ConfigCapability>();
    const std::string default_root = "fuchsia-boot:///dtr#meta/test-parent-sys.cm";
    configurations.push_back({
        .name = "fuchsia.driver.manager.RootDriver",
        .value = request.args().root_driver().value_or(default_root),
    });
    realm_builder_.AddConfiguration(std::move(configurations));
    realm_builder_.AddRoute({
        .capabilities =
            {
                component_testing::Config{.name = "fuchsia.driver.manager.RootDriver"},
            },
        .source = component_testing::SelfRef{},
        .targets = {component_testing::ChildRef{"driver_manager"}},
    });

    realm_ = realm_builder_.SetRealmName("0").Build(dispatcher_);

    // Forward all other protocols.
    exposed_dir_ =
        fidl::ClientEnd<fuchsia_io::Directory>(realm_->component().CloneExposedDir().TakeChannel());

    for (auto& [dir, flags, server_end] : directories_) {
      zx_status_t status = fdio_open_at(exposed_dir_.channel().get(), dir, flags,
                                        server_end.TakeChannel().release());
      if (status != ZX_OK) {
        completer.Reply(zx::error(status));
        return;
      }
    }

    if (request.args().exposes().has_value()) {
      for (const auto& expose : *request.args().exposes()) {
        auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
        if (endpoints.is_error()) {
          completer.Reply(endpoints.take_error());
          return;
        }
        auto flags = static_cast<uint32_t>(fio::OpenFlags::kRightReadable |
                                           fio::wire::OpenFlags::kDirectory);
        zx_status_t status =
            fdio_open_at(exposed_dir_.channel().get(), expose.service_name().c_str(), flags,
                         endpoints->server.TakeChannel().release());
        if (status != ZX_OK) {
          completer.Reply(zx::error(status));
          return;
        }
        auto result =
            outgoing_->AddDirectoryAt(std::move(endpoints->client), "svc", expose.service_name());
        if (result.is_error()) {
          completer.Reply(result.take_error());
          return;
        }
      }
    }

    // Connect all requests that came in before Start was triggered.
    while (cb_queue_.empty() == false) {
      cb_queue_.back()();
      cb_queue_.pop_back();
    };

    completer.Reply(zx::ok());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_driver_test::Realm> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    std::string method_type;
    switch (metadata.unknown_method_type) {
      case fidl::UnknownMethodType::kOneWay:
        method_type = "one-way";
        break;
      case fidl::UnknownMethodType::kTwoWay:
        method_type = "two-way";
        break;
    };

    FX_SLOG(WARNING, "DriverDevelopmentService received unknown method.",
            FX_KV("Direction", method_type.c_str()), FX_KV("Ordinal", metadata.method_ordinal));
  }

 private:
  struct BootAndBaseConfigResult {
    std::vector<std::string> boot_drivers;
    std::vector<std::string> base_drivers;
  };
  static zx::result<BootAndBaseConfigResult> ConstructBootAndBaseConfig(
      fidl::UnownedClientEnd<fuchsia_io::Directory> boot_dir,
      fidl::UnownedClientEnd<fuchsia_io::Directory> pkg_drivers_dir) {
    auto list = std::vector<
        std::tuple<fidl::UnownedClientEnd<fuchsia_io::Directory>, std::string, std::string>>{
        std::make_tuple(boot_dir, "fuchsia-boot:///", "boot"),
    };
    std::unordered_map<std::string, std::vector<std::string>> results;
    if (pkg_drivers_dir.is_valid()) {
      list.emplace_back(pkg_drivers_dir, "fuchsia-pkg://fuchsia.com/", "pkg");
    } else {
      results["pkg"] = {};
    }

    for (const auto& [dir, url_prefix, type] : list) {
      // Check each manifest to see if it uses the driver runner.
      zx::result cloned_dir = component::Clone(dir);
      if (cloned_dir.is_error()) {
        FX_SLOG(ERROR, "Unable to clone dir");
        return zx::error(ZX_ERR_IO);
      }
      fbl::unique_fd dir_fd;
      zx_status_t status =
          fdio_fd_create(cloned_dir->TakeHandle().release(), dir_fd.reset_and_get_address());
      if (status != ZX_OK) {
        FX_SLOG(ERROR, "Failed to turn dir into fd");
        return zx::error(ZX_ERR_IO);
      }
      std::vector<std::string> manifests;
      if (!files::ReadDirContentsAt(dir_fd.get(), "meta", &manifests)) {
        FX_SLOG(WARNING, "Unable to dir contents for ",
                FX_KV("dir", fxl::Concatenate({"/", type, "/meta"})));
      }
      std::vector<std::string> driver_components;
      for (const auto& manifest : manifests) {
        std::string manifest_path = "meta/" + manifest;
        if (!files::IsFileAt(dir_fd.get(), manifest_path) ||
            !cpp20::ends_with(std::string_view(manifest), ".cm")) {
          continue;
        }
        std::vector<uint8_t> manifest_bytes;
        if (!files::ReadFileToVectorAt(dir_fd.get(), manifest_path, &manifest_bytes)) {
          FX_SLOG(ERROR, "Unable to read file contents for", FX_KV("manifest", manifest_path));
          return zx::error(ZX_ERR_IO);
        }
        fit::result component = fidl::Unpersist<fuchsia_component_decl::Component>(manifest_bytes);
        if (component.is_error()) {
          FX_SLOG(ERROR, "Unable to unpersist component manifest",
                  FX_KV("manifest", manifest_path));
          return zx::error(ZX_ERR_IO);
        }
        if (!component->program() || !component->program()->runner() ||
            *component->program()->runner() != "driver") {
          continue;
        }

        // We add a fake package name of dtr to make it identifiable.
        std::string entry = fxl::Substitute("$0dtr#meta/$1", url_prefix, manifest);
        driver_components.push_back(entry);
      }

      results[type] = std::move(driver_components);
    }

    return zx::ok(BootAndBaseConfigResult{
        .boot_drivers = std::move(results["boot"]),
        .base_drivers = std::move(results["pkg"]),
    });
  }

  bool is_started_ = false;
  component::OutgoingDirectory* outgoing_;
  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fuchsia_driver_test::Realm> bindings_;

  struct Directory {
    const char* name;
    uint32_t flags;
    fidl::ServerEnd<fuchsia_io::Directory> server_end;
  };

  std::array<Directory, 2> directories_ = {
      Directory{
          .name = "dev-class",
          .flags =
              static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kDirectory),
          .server_end = {},
      },
      Directory{
          .name = "dev-topological",
          .flags =
              static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kDirectory),
          .server_end = {},
      },
  };

  component_testing::RealmBuilder realm_builder_ =
      component_testing::RealmBuilder::CreateFromRelativeUrl("#meta/test_realm.cm");
  std::optional<component_testing::RealmRoot> realm_;
  fidl::ClientEnd<fuchsia_io::Directory> exposed_dir_;
  // Queue of connection requests that need to be ran once exposed_dir_ is valid.
  std::vector<fit::closure> cb_queue_;
  driver_test_realm_config::Config config_;
};

}  // namespace

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  component::OutgoingDirectory outgoing(loop.dispatcher());

  auto config = driver_test_realm_config::Config::TakeFromStartupHandle();

  DriverTestRealm dtr(&outgoing, loop.dispatcher(), config);
  {
    zx::result result = dtr.Init();
    ZX_ASSERT(result.is_ok());
  }

  {
    zx::result result = outgoing.ServeFromStartupInfo();
    ZX_ASSERT(result.is_ok());
  }

  loop.Run();
  return 0;
}
