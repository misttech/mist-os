// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component.resolution/cpp/wire.h>
#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/markers.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <fidl/fuchsia.pkg/cpp/wire.h>
#include <fidl/fuchsia.system.state/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/stdcompat/string_view.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/syslog/global.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/job.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <format>
#include <list>
#include <memory>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include <ddk/metadata/test.h>
#include <fbl/string_printf.h>
#include <fbl/unique_fd.h>
#include <rapidjson/document.h>
#include <sdk/lib/driver_test_realm/driver_test_realm_config.h>
#include <src/lib/files/directory.h>
#include <src/lib/files/file.h>

namespace {

namespace fio = fuchsia_io;
namespace fdt = fuchsia_driver_test;
namespace fres = fuchsia_component_resolution;
namespace fpkg = fuchsia_pkg;

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
      return ZX_ERR_NOT_FOUND;
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
      FX_LOG_KV(WARNING, "Failed to get boot items", FX_KV("status", status));
    }
    completer.Reply(std::move(vmo), length);
  }
  void Get2(Get2RequestView request, Get2Completer::Sync& completer) override {
    std::vector<board_test::DeviceEntry> entries = {};
    zx::vmo vmo;
    uint32_t length = 0;
    uint32_t extra = 0;
    zx_status_t status = GetBootItem(entries, request->type, board_name_, extra, &vmo, &length);
    if (status != ZX_OK) {
      FX_LOG_KV(WARNING, "Failed to get boot items", FX_KV("status", status));
      completer.Reply(zx::error(status));
      return;
    }
    std::vector<fuchsia_boot::wire::RetrievedItems> result;
    fuchsia_boot::wire::RetrievedItems items = {
        .payload = std::move(vmo), .length = length, .extra = extra};
    result.emplace_back(std::move(items));
    completer.ReplySuccess(
        fidl::VectorView<fuchsia_boot::wire::RetrievedItems>::FromExternal(result));
  }

  void GetBootloaderFile(GetBootloaderFileRequestView request,
                         GetBootloaderFileCompleter::Sync& completer) override {
    completer.Reply(zx::vmo());
  }

  std::string board_name_;
};

class FakeSystemStateTransition final
    : public fidl::WireServer<fuchsia_system_state::SystemStateTransition> {
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override {
    completer.Reply(fuchsia_system_state::SystemPowerState::kFullyOn);
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
      FX_LOG_KV(ERROR, "Failed to duplicate job", FX_KV("status", status));
    }
    completer.Reply(std::move(job));
  }
};

class InternalServer final : public fidl::WireServer<fuchsia_driver_test::Internal> {
 public:
  InternalServer(std::optional<fres::Context> context,
                 fidl::ClientEnd<fuchsia_io::Directory> test_pkg_dir)
      : context_(std::move(context)), test_pkg_dir_(std::move(test_pkg_dir)) {}

  void GetTestPackage(GetTestPackageCompleter::Sync& completer) override {
    auto dir_clone = component::Clone(test_pkg_dir_);
    if (dir_clone.is_ok()) {
      completer.ReplySuccess(std::move(*dir_clone));
    } else {
      completer.ReplyError(dir_clone.error_value());
    }
  }

  void GetTestResolutionContext(GetTestResolutionContextCompleter::Sync& completer) override {
    fidl::Arena arena;
    if (context_.has_value()) {
      completer.ReplySuccess(fidl::ToWire(arena, *context_));
    } else {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
    }
  }

 private:
  std::optional<fres::Context> context_;
  fidl::ClientEnd<fuchsia_io::Directory> test_pkg_dir_;
};

zx::result<fidl::ClientEnd<fio::Directory>> OpenPkgDir() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  zx_status_t status = fdio_open3(
      "/pkg",
      static_cast<uint64_t>(fuchsia_io::wire::Flags::kProtocolDirectory |
                            fuchsia_io::wire::kPermReadable | fuchsia_io::wire::kPermExecutable),
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
    // Set up realm_builder_exposed_dir now so that we can queue up requests before `Start` has been
    // called.  When `Start` has been called, we'll connect the directory to the exposed directory
    // of the started realm.
    {
      constexpr std::string_view kRealmBuilderExposedDir = "realm_builder_exposed_dir";

      zx::result client_end = fidl::CreateEndpoints(&realm_builder_exposed_dir_);
      if (client_end.is_error()) {
        return client_end.take_error();
      }

      zx::result result =
          outgoing_->AddDirectory(std::move(client_end.value()), kRealmBuilderExposedDir);
      if (result.is_error()) {
        FX_LOG_KV(ERROR, "Failed to add directory to outgoing directory",
                  FX_KV("directory", kRealmBuilderExposedDir));
        return result.take_error();
      }
    }

    // Hook up fuchsia.driver.test/Realm so we can proceed with the rest of initialization once
    // |Start| is invoked
    zx::result result = outgoing_->AddUnmanagedProtocol<fuchsia_driver_test::Realm>(
        bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
    if (result.is_error()) {
      FX_LOG_KV(ERROR, "Failed to add protocol to outgoing directory",
                FX_KV("protocol", "fuchsia.driver.test/Realm"));
      return result.take_error();
    }

    return zx::ok();
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    // Non-hermetic users will end up calling start several times as the component test framework
    // invokes the binary multiple times, resulting in main running several times. We may be
    // ignoring real issues by ignoring the subsequent calls in the case that multiple parties
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

    zx::result result = outgoing_->AddProtocol<fuchsia_system_state::SystemStateTransition>(
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

    // Setup /base_drivers
    fidl::ClientEnd<fuchsia_io::Directory> base_drivers_dir;
    if (request.args().pkg().has_value()) {
      base_drivers_dir = fidl::ClientEnd<fuchsia_io::Directory>(std::move(*request.args().pkg()));
    } else {
      auto res = OpenPkgDir();
      if (res.is_error()) {
        completer.Reply(res.take_error());
        return;
      }
      base_drivers_dir = std::move(res.value());
    }

    // Look at the test's component package and subpackages.
    fidl::ClientEnd<fuchsia_io::Directory> test_pkg_dir;
    std::optional<fres::Context> test_resolution_context;
    if (request.args().test_component().has_value()) {
      test_resolution_context = request.args().test_component()->resolution_context();
      test_pkg_dir = std::move(*request.args().test_component()->package()->directory());
    }

    // We only index /pkg if it's not identical to /boot.
    const bool create_pkg_config = request.args().pkg() || request.args().boot();

    auto boot_driver_components =
        request.args().boot_driver_components().value_or(std::vector<std::string>{});

    std::unordered_set<std::string> boot_driver_components_set(boot_driver_components.begin(),
                                                               boot_driver_components.end());

    auto client_list = MakeClientList(
        boot_dir,
        create_pkg_config ? base_drivers_dir : fidl::UnownedClientEnd<fuchsia_io::Directory>({}),
        test_pkg_dir, test_resolution_context);

    if (client_list.is_error()) {
      completer.Reply(client_list.take_error());
      return;
    }

    zx::result base_and_boot_configs =
        ConstructBootAndBaseConfig(*std::move(client_list), boot_driver_components_set);
    if (base_and_boot_configs.is_error()) {
      completer.Reply(base_and_boot_configs.take_error());
      return;
    }

    result = outgoing_->AddProtocol<fuchsia_driver_test::Internal>(
        std::make_unique<InternalServer>(test_resolution_context, std::move(test_pkg_dir)));
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    result = outgoing_->AddDirectory(std::move(boot_dir), "boot");
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    result = outgoing_->AddDirectory(std::move(base_drivers_dir), "base_drivers");
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
                CollectionRef{"base-drivers"},
                CollectionRef{"full-drivers"},
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

    if (request.args().dtr_offers()) {
      for (const auto& offer_cap : *request.args().dtr_offers()) {
        std::optional<Capability> converted;
        switch (offer_cap.Which()) {
          case fuchsia_component_test::Capability::Tag::kProtocol: {
            const auto& offer_cap_proto = offer_cap.protocol().value();
            converted.emplace(Protocol{
                offer_cap_proto.name().value(),
                offer_cap_proto.as(),
                offer_cap_proto.type().has_value() ? std::make_optional(static_cast<DependencyType>(
                                                         offer_cap_proto.type().value()))
                                                   : std::nullopt,
                offer_cap_proto.path(),
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
                offer_cap_proto.from_dictionary(),
#endif
            });
            break;
          }
          case fuchsia_component_test::Capability::Tag::kConfig: {
            const auto& offer_cap_config = offer_cap.config().value();
            converted.emplace(Config{
                offer_cap_config.name().value(),
                offer_cap_config.as(),
            });
            break;
          }
          case fuchsia_component_test::Capability::Tag::kDirectory:
          case fuchsia_component_test::Capability::Tag::kStorage:
          case fuchsia_component_test::Capability::Tag::kService:
          case fuchsia_component_test::Capability::Tag::kEventStream:
          case fuchsia_component_test::Capability::Tag::kDictionary:
          default:
            FX_LOG_KV(WARNING, "Skipping unsupported offer capability.",
                      FX_KV("type", static_cast<uint64_t>(offer_cap.Which())));
            break;
        }

        if (converted.has_value()) {
          realm_builder_.AddRoute(Route{
              .capabilities = {converted.value()},
              .source = {ParentRef()},
              .targets =
                  {
                      CollectionRef{"boot-drivers"},
                      CollectionRef{"base-drivers"},
                      CollectionRef{"full-drivers"},
                  },
          });
        }
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

    if (request.args().dtr_exposes()) {
      for (const auto& expose_cap : *request.args().dtr_exposes()) {
        std::optional<Capability> converted;
        switch (expose_cap.Which()) {
          case fuchsia_component_test::Capability::Tag::kService: {
            const auto& expose_cap_service = expose_cap.service().value();
            converted.emplace(Service{
                expose_cap_service.name().value(),
                expose_cap_service.as(),
                expose_cap_service.path(),
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
                expose_cap_service.from_dictionary(),
#endif
            });
            break;
          }
          case fuchsia_component_test::Capability::Tag::kProtocol:
          case fuchsia_component_test::Capability::Tag::kDirectory:
          case fuchsia_component_test::Capability::Tag::kStorage:
          case fuchsia_component_test::Capability::Tag::kEventStream:
          case fuchsia_component_test::Capability::Tag::kConfig:
          case fuchsia_component_test::Capability::Tag::kDictionary:
          default:
            FX_LOG_KV(WARNING, "Skipping unsupported expose capability.",
                      FX_KV("type", static_cast<uint64_t>(expose_cap.Which())));
            break;
        }

        if (converted.has_value()) {
          realm_builder_.AddRoute(Route{
              .capabilities = {converted.value()},
              .source =
                  {
                      CollectionRef{"boot-drivers"},
                  },
              .targets = {ParentRef()},
          });
          realm_builder_.AddRoute(Route{
              .capabilities = {converted.value()},
              .source =
                  {
                      CollectionRef{"base-drivers"},
                  },
              .targets = {ParentRef()},
          });
          realm_builder_.AddRoute(Route{
              .capabilities = {converted.value()},
              .source =
                  {
                      CollectionRef{"full-drivers"},
                  },
              .targets = {ParentRef()},
          });
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
    configurations.push_back({
        .name = "fuchsia.driver.index.StopOnIdleTimeoutMillis",
        .value = ConfigValue::Int64(request.args().driver_index_stop_timeout_millis().value_or(-1)),
    });
    realm_builder_.AddConfiguration(std::move(configurations));
    realm_builder_.AddRoute({
        .capabilities =
            {
                component_testing::Config{.name = "fuchsia.driver.BootDrivers"},
                component_testing::Config{.name = "fuchsia.driver.BaseDrivers"},
                component_testing::Config{.name = "fuchsia.driver.BindEager"},
                component_testing::Config{.name = "fuchsia.driver.DisabledDrivers"},
                component_testing::Config{.name = "fuchsia.driver.index.StopOnIdleTimeoutMillis"},
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

    // Set platform bus config based on request.
    configurations = std::vector<component_testing::ConfigCapability>();
    component_testing::Ref source;
    if (request.args().software_devices()) {
      source = component_testing::SelfRef();
      std::vector<std::string> device_names;
      std::vector<uint32_t> device_ids;
      for (const auto& device : *request.args().software_devices()) {
        device_names.push_back(device.device_name());
        device_ids.push_back(device.device_id());
      }
      configurations.push_back({
          .name = "fuchsia.platform.bus.SoftwareDeviceNames",
          .value = ConfigValue(device_names),
      });
      configurations.push_back({
          .name = "fuchsia.platform.bus.SoftwareDeviceIds",
          .value = ConfigValue(device_ids),
      });
    } else {
      source = component_testing::VoidRef();
    }
    realm_builder_.AddConfiguration(std::move(configurations));
    realm_builder_.AddRoute({
        .capabilities =
            {
                component_testing::Config{.name = "fuchsia.platform.bus.SoftwareDeviceNames"},
                component_testing::Config{.name = "fuchsia.platform.bus.SoftwareDeviceIds"},
            },
        .source = source,
        .targets = {component_testing::CollectionRef{"boot-drivers"}},
    });

    realm_ = realm_builder_.SetRealmName("0").Build(dispatcher_);

    // Forward exposes.
    auto exposed_dir = realm_->component().exposed().unowned_channel()->get();
    if (request.args().exposes().has_value()) {
      for (const auto& expose : *request.args().exposes()) {
        auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
        if (endpoints.is_error()) {
          completer.Reply(endpoints.take_error());
          return;
        }
        auto flags =
            static_cast<uint64_t>(fio::kPermReadable | fio::wire::Flags::kProtocolDirectory);
        zx_status_t status = fdio_open3_at(exposed_dir, expose.service_name().c_str(), flags,
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

    // Connect realm_builder_exposed_dir.
    if (zx_status_t status =
            fdio_open3_at(exposed_dir, ".",
                          static_cast<uint64_t>(fio::kPermReadable | fio::Flags::kPermInheritWrite |
                                                fio::Flags::kPermInheritExecute),
                          realm_builder_exposed_dir_.TakeChannel().release());
        status != ZX_OK) {
      completer.Reply(zx::error(status));
      return;
    }

    // Connect to the driver manager and wait for bootup to complete before returning.
    auto manager = component::ConnectAt<fuchsia_driver_development::Manager>(
        fidl::UnownedClientEnd<fuchsia_io::Directory>(
            realm_->component().exposed().unowned_channel()));
    if (manager.is_error()) {
      completer.Reply(manager.take_error());
      return;
    }

    development_manager_client_.Bind(*std::move(manager), dispatcher_);

    development_manager_client_->WaitForBootup().Then(
        [completer = completer.ToAsync()](
            fidl::Result<fuchsia_driver_development::Manager::WaitForBootup>& wait_result) mutable {
          if (wait_result.is_error()) {
            completer.Reply(zx::error(wait_result.error_value().status()));
            return;
          }

          completer.Reply(zx::ok());
        });
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

    FX_LOG_KV(WARNING, "DriverDevelopmentService received unknown method.",
              FX_KV("Direction", method_type.c_str()), FX_KV("Ordinal", metadata.method_ordinal));
  }

 private:
  struct BootAndBaseConfigResult {
    std::vector<std::string> boot_drivers;
    std::vector<std::string> base_drivers;
  };

  struct ClientListEntry {
    fidl::ClientEnd<fuchsia_io::Directory> client;
    std::string url_prefix;
    std::string pkg_name;
  };

  static zx::result<std::list<ClientListEntry>> MakeClientList(
      fidl::UnownedClientEnd<fuchsia_io::Directory> boot_drivers_dir,
      fidl::UnownedClientEnd<fuchsia_io::Directory> base_drivers_dir,
      fidl::UnownedClientEnd<fuchsia_io::Directory> test_pkg_dir,
      const std::optional<fres::Context>& test_resolution_context) {
    std::list<ClientListEntry> client_list;

    zx::result cloned_boot_drivers_dir = component::Clone(boot_drivers_dir);
    if (cloned_boot_drivers_dir.is_error()) {
      FX_LOG_KV(ERROR, "Unable to clone dir");
      return zx::error(ZX_ERR_IO);
    }

    client_list.emplace_back(ClientListEntry{
        *std::move(cloned_boot_drivers_dir),
        "fuchsia-boot:///",
        "dtr",
    });

    if (base_drivers_dir.is_valid()) {
      zx::result cloned_base_drivers_dir = component::Clone(base_drivers_dir);
      if (cloned_base_drivers_dir.is_error()) {
        FX_LOG_KV(ERROR, "Unable to clone dir");
        return zx::error(ZX_ERR_IO);
      }

      client_list.emplace_back(ClientListEntry{
          *std::move(cloned_base_drivers_dir),
          "fuchsia-pkg://fuchsia.com/",
          "dtr",
      });
    } else if (test_pkg_dir.is_valid()) {
      zx::result cloned_test_pkg_dir = component::Clone(test_pkg_dir);
      if (cloned_test_pkg_dir.is_error()) {
        FX_LOG_KV(ERROR, "Unable to clone dir");
        return zx::error(ZX_ERR_IO);
      }

      // Add the test package itself.
      client_list.emplace_back(ClientListEntry{
          *std::move(cloned_test_pkg_dir),
          "dtr-test-pkg://fuchsia.com/",
          "",
      });

      // Need to clone again to use in fdio_fd_create.
      cloned_test_pkg_dir = component::Clone(test_pkg_dir);
      if (cloned_test_pkg_dir.is_error()) {
        FX_LOG_KV(ERROR, "Unable to clone dir");
        return zx::error(ZX_ERR_IO);
      }
      fbl::unique_fd dir_fd;
      zx_status_t status = fdio_fd_create(cloned_test_pkg_dir->TakeHandle().release(),
                                          dir_fd.reset_and_get_address());
      if (status != ZX_OK) {
        FX_LOG_KV(ERROR, "Failed to turn dir into fd");
        return zx::error(ZX_ERR_IO);
      }

      if (files::IsFileAt(dir_fd.get(), "meta/fuchsia.pkg/subpackages")) {
        // Read off the subpackages that exist in the test package.
        std::string result;
        files::ReadFileToStringAt(dir_fd.get(), "meta/fuchsia.pkg/subpackages", &result);
        rapidjson::Document subpackages_doc;
        subpackages_doc.Parse(result.c_str());
        auto& subpackages = subpackages_doc["subpackages"];
        std::vector<std::string> subpackage_names;
        subpackage_names.reserve(subpackages.MemberCount());
        for (rapidjson::Value::ConstMemberIterator itr = subpackages.MemberBegin();
             itr != subpackages.MemberEnd(); ++itr) {
          subpackage_names.push_back(itr->name.GetString());
        }

        if (!subpackage_names.empty()) {
          // Resolve the subpackage using the context.
          auto resolver = component::Connect<fpkg::PackageResolver>(
              "/svc/fuchsia.pkg.PackageResolver-hermetic");
          if (resolver.is_error()) {
            FX_LOG_KV(ERROR, "Failed to connect to resolver protocol.",
                      FX_KV("error", zx_status_get_string(resolver.error_value())));
            return resolver.take_error();
          }
          if (!test_resolution_context.has_value()) {
            FX_LOG_KV(
                WARNING,
                "Subpackages exist in this package but a resolution context was not provided with the test_component. Skipping.");
          } else {
            fidl::Arena arena;
            fuchsia_pkg::wire::ResolutionContext converted_context(
                {.bytes = fidl::ToWire(arena, test_resolution_context->bytes())});

            // Add the subpackages of the test package into the list.
            for (auto& subpackage_name : subpackage_names) {
              auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
              auto resolve_result = fidl::WireCall(*resolver)->ResolveWithContext(
                  fidl::StringView(arena, subpackage_name), converted_context, std::move(server));
              if (!resolve_result.ok() || resolve_result->is_error()) {
                FX_LOG_KV(ERROR, "Failed to resolve_with_context in the test package.",
                          FX_KV("error", resolve_result.FormatDescription().c_str()));
                return zx::error(ZX_ERR_INTERNAL);
              }

              client_list.emplace_back(ClientListEntry{
                  std::move(client),
                  "dtr-test-pkg://fuchsia.com/",
                  subpackage_name,
              });
            }
          }
        }
      }
    }

    return zx::ok(std::move(client_list));
  }

  static zx::result<BootAndBaseConfigResult> ConstructBootAndBaseConfig(
      std::list<ClientListEntry> client_list,
      const std::unordered_set<std::string>& boot_driver_components) {
    std::unordered_set<std::string> inserted;
    std::vector<std::string> boot_drivers;
    std::vector<std::string> base_drivers;

    for (auto& [dir, url_prefix, pkg_name] : client_list) {
      // Check each manifest to see if it uses the driver runner.
      fbl::unique_fd dir_fd;
      zx_status_t status =
          fdio_fd_create(dir.TakeHandle().release(), dir_fd.reset_and_get_address());
      if (status != ZX_OK) {
        FX_LOG_KV(ERROR, "Failed to turn dir into fd");
        return zx::error(ZX_ERR_IO);
      }

      std::vector<std::string> manifests;
      if (!files::ReadDirContentsAt(dir_fd.get(), "meta", &manifests)) {
        FX_LOG_KV(WARNING, "Unable to read dir contents.", FX_KV("url_prefix", url_prefix),
                  FX_KV("pkg_name", pkg_name));
        continue;
      }

      for (const auto& manifest : manifests) {
        std::string manifest_path = "meta/" + manifest;
        if (!files::IsFileAt(dir_fd.get(), manifest_path) ||
            !cpp20::ends_with(std::string_view(manifest), ".cm")) {
          continue;
        }
        std::vector<uint8_t> manifest_bytes;
        if (!files::ReadFileToVectorAt(dir_fd.get(), manifest_path, &manifest_bytes)) {
          FX_LOG_KV(ERROR, "Unable to read file contents for", FX_KV("manifest", manifest_path));
          return zx::error(ZX_ERR_IO);
        }
        fit::result component = fidl::Unpersist<fuchsia_component_decl::Component>(manifest_bytes);
        if (component.is_error()) {
          FX_LOG_KV(ERROR, "Unable to unpersist component manifest",
                    FX_KV("manifest", manifest_path));
          return zx::error(ZX_ERR_IO);
        }
        if (!component->program() || !component->program()->runner() ||
            *component->program()->runner() != "driver") {
          continue;
        }

        // Construct the url entry from the pieces provided in the list entry.
        std::string entry = std::format("{}{}#meta/{}", url_prefix, pkg_name, manifest);

        // Protect against duplicate drivers. Only add the driver if it is unique.
        if (inserted.insert(std::format("{}/{}", pkg_name, manifest)).second) {
          // Add to corresponding list.
          if (entry.starts_with("fuchsia-boot:///") ||
              boot_driver_components.find(manifest) != boot_driver_components.end()) {
            boot_drivers.push_back(entry);
            FX_LOG_KV(INFO, "driver test realm added boot driver: ", FX_KV("entry", entry.c_str()));
          } else {
            base_drivers.push_back(entry);
            FX_LOG_KV(INFO, "driver test realm added base driver: ", FX_KV("entry", entry.c_str()));
          }
        }
      }
    }

    return zx::ok(BootAndBaseConfigResult{
        .boot_drivers = std::move(boot_drivers),
        .base_drivers = std::move(base_drivers),
    });
  }

  bool is_started_ = false;
  component::OutgoingDirectory* outgoing_;
  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fuchsia_driver_test::Realm> bindings_;
  fidl::Client<fuchsia_driver_development::Manager> development_manager_client_;

  struct Directory {
    const char* name;
    uint32_t flags;
    fidl::ServerEnd<fuchsia_io::Directory> server_end;
  };

  component_testing::RealmBuilder realm_builder_ =
      component_testing::RealmBuilder::CreateFromRelativeUrl("#meta/test_realm.cm");
  std::optional<component_testing::RealmRoot> realm_;
  driver_test_realm_config::Config config_;
  fidl::ServerEnd<fuchsia_io::Directory> realm_builder_exposed_dir_;
};

}  // namespace

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithDispatcher(loop.dispatcher()).BuildAndInitialize();
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
