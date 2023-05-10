// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.boot/cpp/wire.h>
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
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/syslog/global.h>
#include <lib/vfs/cpp/remote_dir.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/job.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>

#include <charconv>
#include <fstream>
#include <memory>
#include <sstream>
#include <unordered_map>
#include <vector>

#include <ddk/metadata/test.h>
#include <fbl/string_printf.h>
#include <mock-boot-arguments/server.h>

#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/pseudo_file.h"
#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"

namespace {

namespace fio = fuchsia_io;
namespace fdt = fuchsia_driver_test;

using namespace component_testing;

const char* LogLevelToString(fuchsia_diagnostics::Severity severity) {
  switch (severity) {
    case fuchsia_diagnostics::Severity::kTrace:
      return "TRACE";
    case fuchsia_diagnostics::Severity::kDebug:
      return "DEBUG";
    case fuchsia_diagnostics::Severity::kInfo:
      return "INFO";
    case fuchsia_diagnostics::Severity::kWarn:
      return "WARN";
    case fuchsia_diagnostics::Severity::kError:
      return "ERROR";
    case fuchsia_diagnostics::Severity::kFatal:
      return "FATAL";
  }
}

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
      FX_SLOG(ERROR, "Failed to get boot items", KV("status", status));
    }
    completer.Reply(std::move(vmo), length);
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
      FX_SLOG(ERROR, "Failed to duplicate job", KV("status", status));
    }
    completer.Reply(std::move(job));
  }
};

class FakeBasePackageResolver final : public fidl::WireServer<fuchsia_pkg::PackageResolver> {
 public:
  FakeBasePackageResolver(fs::SynchronousVfs* vfs) : vfs_(vfs) {}

 private:
  void Resolve(ResolveRequestView request, ResolveCompleter::Sync& completer) override {
    if (request->package_url.get() != "fuchsia-pkg://fuchsia.com/driver-manager-base-config" &&
        request->package_url.get() != "fuchsia-pkg://fuchsia.com/driver-manager-base-config/0") {
      FX_SLOG(ERROR, "FakeBasePackageResolver asked to resolve unknown package",
              KV("url", request->package_url.get()));
      completer.ReplyError(fuchsia_pkg::wire::ResolveError::kPackageNotFound);
      return;
    }

    auto driver_manager_base_config = fbl::MakeRefCounted<fs::PseudoDir>();
    // Create the driver-manager-base-config package.
    // It has entry /config/base-driver-manifest.json
    {
      auto config = fbl::MakeRefCounted<fs::PseudoDir>();
      auto base_driver_manifest = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>(
          [](fbl::String* output) {
            // Return an empty JSON array.
            *output = fbl::String("[]");
            return ZX_OK;
          },
          [](std::string_view input) { return ZX_ERR_NOT_SUPPORTED; });

      config->AddEntry("base-driver-manifest.json", std::move(base_driver_manifest));
      driver_manager_base_config->AddEntry("config", std::move(config));
    }

    if (zx_status_t status =
            vfs_->Serve(std::move(driver_manager_base_config), request->dir.TakeChannel(),
                        fs::VnodeConnectionOptions::ReadExec());
        status != ZX_OK) {
      FX_SLOG(ERROR, "FakeBasePackageResolver failed to serve driver-manager-base-config",
              KV("status", status));
      completer.ReplyError(fuchsia_pkg::wire::ResolveError::kInternal);
    }

    auto resolution_context = fuchsia_pkg::wire::ResolutionContext({});
    completer.ReplySuccess(resolution_context);
  }

  void ResolveWithContext(ResolveWithContextRequestView request,
                          ResolveWithContextCompleter::Sync& completer) override {
    FX_SLOG(ERROR, "ResolveWithContext is not yet implemented in FakeBasePackageResolver");
    completer.ReplyError(fuchsia_pkg::wire::ResolveError::kInternal);
  }

  void GetHash(GetHashRequestView request, GetHashCompleter::Sync& completer) override {
    // Even given the same URL, GetHash is not guaranteed to return the hash of the previously
    // resolved package (e.g. the package repository could have been updated in between calls).
    // Clients should obtain the package's hash by reading the package's meta entry as a file.
    FX_SLOG(ERROR, "GetHash is not implemented in FakeBasePackageResolver");
    completer.ReplyError(ZX_ERR_INTERNAL);
  }

  fs::SynchronousVfs* vfs_;
};

class FakeFullPackageResolver final : public fidl::WireServer<fuchsia_pkg::PackageResolver> {
  void Resolve(ResolveRequestView request, ResolveCompleter::Sync& completer) override {
    auto status = fdio_open("/pkg",
                            static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory |
                                                  fuchsia_io::wire::OpenFlags::kRightReadable |
                                                  fuchsia_io::wire::OpenFlags::kRightExecutable),
                            request->dir.TakeChannel().release());
    if (status != ZX_OK) {
      completer.ReplyError(fuchsia_pkg::wire::ResolveError::kInternal);
      return;
    }

    auto resolution_context = fuchsia_pkg::wire::ResolutionContext({});
    completer.ReplySuccess(resolution_context);
  }

  void ResolveWithContext(ResolveWithContextRequestView request,
                          ResolveWithContextCompleter::Sync& completer) override {
    FX_SLOG(ERROR, "ResolveWithContext is not yet implemented in FakeFullPackageResolver");
    completer.ReplyError(fuchsia_pkg::wire::ResolveError::kInternal);
  }

  void GetHash(GetHashRequestView request, GetHashCompleter::Sync& completer) override {
    // Even given the same URL, GetHash is not guaranteed to return the hash of the previously
    // resolved package (e.g. the package repository could have been updated in between calls).
    // Clients should obtain the package's hash by reading the package's meta entry as a file.
    FX_SLOG(ERROR, "GetHash is not implemented in FakeFullPackageResolver");
    completer.ReplyError(ZX_ERR_INTERNAL);
  }
};

std::map<std::string, std::string> CreateBootArgs(const fuchsia_driver_test::RealmArgs& args) {
  std::map<std::string, std::string> boot_args;

  if (args.driver_tests_enable_all().has_value() && *args.driver_tests_enable_all()) {
    boot_args["driver.tests.enable"] = "true";
  }

  if (args.driver_tests_enable().has_value()) {
    for (const auto& driver : *args.driver_tests_enable()) {
      auto string = fbl::StringPrintf("driver.%s.tests.enable", driver.c_str());
      boot_args[string.data()] = "true";
    }
  }

  if (args.driver_tests_disable().has_value()) {
    for (const auto& driver : *args.driver_tests_disable()) {
      auto string = fbl::StringPrintf("driver.%s.tests.enable", driver.c_str());
      boot_args[string.data()] = "false";
    }
  }

  if (args.driver_log_level().has_value()) {
    for (const auto& driver : *args.driver_log_level()) {
      auto string = fbl::StringPrintf("driver.%s.log", driver.name().c_str());
      boot_args[string.data()] = LogLevelToString(driver.log_level());
    }
  }

  if (args.driver_disable().has_value()) {
    std::vector<std::string_view> drivers(args.driver_disable()->size());
    for (const auto& driver : *args.driver_disable()) {
      drivers.emplace_back(driver);
      auto string = fbl::StringPrintf("driver.%s.disable", driver.c_str());
      boot_args[string.data()] = "true";
    }
  }

  return boot_args;
}

class DriverTestRealm final : public fidl::Server<fuchsia_driver_test::Realm> {
 public:
  DriverTestRealm(component::OutgoingDirectory* outgoing, async_dispatcher_t* dispatcher)
      : outgoing_(outgoing), dispatcher_(dispatcher), vfs_(dispatcher_) {}

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
        FX_SLOG(ERROR, "Failed to add directory to outgoing directory", KV("directory", dir));
        return result.take_error();
      }
    }

    const std::array<std::string, 5> kProtocols = {
        "fuchsia.device.manager.Administrator",     "fuchsia.driver.development.DriverDevelopment",
        "fuchsia.driver.registrar.DriverRegistrar", "fuchsia.hardware.pci.DeviceWatcher",
        "fuchsia.hardware.usb.DeviceWatcher",
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
                KV("protocol", protocol.c_str()));
        return result.take_error();
      }
    }

    // Hook up fuchsia.driver.test/Realm so we can proceed with the rest of initialization once
    // |Start| is invoked
    zx::result result = outgoing_->AddUnmanagedProtocol<fuchsia_driver_test::Realm>(
        bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
    if (result.is_error()) {
      FX_SLOG(ERROR, "Failed to add protocol to outgoing directory",
              KV("protocol", "fuchsia.driver.test/Realm"));
      return result.take_error();
    }

    return zx::ok();
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    // Non-hermetic users will end up calling start several times as the component test framework
    // invokes the binary multiple times, resulting in main running several times. We may be
    // ignoring real issues by ignoreing the subsequent calls in the case that multiple parties
    // are invoking start unknowingly. Comparing the args may be a way to avoid that issue.
    // TODO(http://fxbug.dev/122136): Remedy this situation
    if (is_started_) {
      completer.Reply(zx::ok());
      return;
    }
    is_started_ = true;

    auto boot_args = CreateBootArgs(request.args());
    for (std::pair<std::string, std::string> boot_arg : boot_args) {
      if (boot_arg.first.size() > fuchsia_boot::wire::kMaxArgsNameLength) {
        FX_SLOG(ERROR, "The length of the name of the boot argument is too long",
                KV("arg", boot_arg.first.data()),
                KV("maximum_length", fuchsia_boot::wire::kMaxArgsNameLength));
        completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
        return;
      }

      if (boot_arg.second.size() > fuchsia_boot::wire::kMaxArgsValueLength) {
        FX_SLOG(ERROR, "The length of the value of the boot argument is too long",
                KV("arg", boot_arg.first.data()), KV("value", boot_arg.second.data()),
                KV("maximum_length", fuchsia_boot::wire::kMaxArgsValueLength));
        completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
        return;
      }
    }
    auto boot_arguments = std::make_unique<mock_boot_arguments::Server>(std::move(boot_args));

    // Add protocols which are routed to realm builder.
    zx::result result = outgoing_->AddProtocol<fuchsia_boot::Arguments>(std::move(boot_arguments));
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    auto boot_items = std::make_unique<FakeBootItems>();
    if (request.args().board_name().has_value()) {
      boot_items->board_name_ = *request.args().board_name();
    }
    result = outgoing_->AddProtocol<fuchsia_boot::Items>(std::move(boot_items));
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    result = outgoing_->AddProtocol<fuchsia_device_manager::SystemStateTransition>(
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

    result = outgoing_->AddProtocol<fuchsia_pkg::PackageResolver>(
        std::make_unique<FakeBasePackageResolver>(&vfs_), "fuchsia.pkg.PackageResolver-base");
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    result = outgoing_->AddProtocol<fuchsia_pkg::PackageResolver>(
        std::make_unique<FakeFullPackageResolver>(), "fuchsia.pkg.PackageResolver-full");
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    fidl::ClientEnd<fuchsia_io::Directory> boot_dir;
    if (request.args().boot().has_value()) {
      boot_dir = fidl::ClientEnd<fuchsia_io::Directory>(std::move(*request.args().boot()));
    } else {
      auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
      if (endpoints.is_error()) {
        completer.Reply(zx::error(ZX_ERR_INTERNAL));
        return;
      }
      zx_status_t status =
          fdio_open("/pkg",
                    static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory |
                                          fuchsia_io::wire::OpenFlags::kRightReadable |
                                          fuchsia_io::wire::OpenFlags::kRightExecutable),
                    endpoints->server.TakeChannel().release());
      if (status != ZX_OK) {
        completer.Reply(zx::error(ZX_ERR_INTERNAL));
        return;
      }
      boot_dir = std::move(endpoints->client);
    }

    result = outgoing_->AddDirectory(std::move(boot_dir), "boot");
    if (result.is_error()) {
      completer.Reply(result.take_error());
      return;
    }

    // Add additional routes if specified.
    std::unordered_map<fdt::Collection, const char*> kMap = {
        {fdt::Collection::kBootDrivers, "boot-drivers"},
        {fdt::Collection::kPackageDrivers, "pkg-drivers"},
    };
    if (request.args().offers().has_value()) {
      for (const auto& offer : *request.args().offers()) {
        realm_builder_.AddRoute(Route{.capabilities = {Protocol{offer.protocol_name()}},
                                      .source = {ParentRef()},
                                      .targets = {CollectionRef{kMap[offer.collection()]}}});
      }
    }

    if (request.args().exposes().has_value()) {
      for (const auto& expose : *request.args().exposes()) {
        realm_builder_.AddRoute(Route{.capabilities = {Service{expose.service_name()}},
                                      .source = {CollectionRef{kMap[expose.collection()]}},
                                      .targets = {ParentRef()}});
      }
    }

    // Set driver-index config based on request.
    realm_builder_.InitMutableConfigFromPackage("driver-index");
    realm_builder_.SetConfigValue("driver-index", "enable_ephemeral_drivers",
                                  ConfigValue::Bool(true));
    realm_builder_.SetConfigValue("driver-index", "delay_fallback_until_base_drivers_indexed",
                                  ConfigValue::Bool(true));
    const std::vector<std::string> kEmptyVec;
    realm_builder_.SetConfigValue("driver-index", "bind_eager",
                                  request.args().driver_bind_eager().value_or(kEmptyVec));
    realm_builder_.SetConfigValue("driver-index", "disabled_drivers",
                                  request.args().driver_disable().value_or(kEmptyVec));

    // Set driver_manager config based on request.
    realm_builder_.InitMutableConfigFromPackage("driver_manager");
    const bool is_dfv2 =
        request.args().use_driver_framework_v2().value_or(USE_DRIVER_FRAMEWORK_V2_DEFAULT);
    realm_builder_.SetConfigValue("driver_manager", "use_driver_framework_v2",
                                  ConfigValue::Bool(is_dfv2));

    const std::string default_root = "fuchsia-boot:///#meta/test-parent-sys.cm";
    realm_builder_.SetConfigValue("driver_manager", "root_driver",
                                  request.args().root_driver().value_or(default_root));

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

 private:
  bool is_started_ = false;
  component::OutgoingDirectory* outgoing_;
  async_dispatcher_t* dispatcher_;
  fs::SynchronousVfs vfs_;
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
};

}  // namespace

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  component::OutgoingDirectory outgoing(loop.dispatcher());

  DriverTestRealm dtr(&outgoing, loop.dispatcher());
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
