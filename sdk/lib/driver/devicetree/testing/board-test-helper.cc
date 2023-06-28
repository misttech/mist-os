// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/testing/board-test-helper.h"

#include <fcntl.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.driver.test/cpp/wire.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fit/defer.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/vfs/cpp/service.h>
#include <lib/zbi-format/zbi.h>
#include <sys/stat.h>
#include <zircon/errors.h>

#include <memory>
#include <vector>

namespace fdf_devicetree::testing {

namespace {

using namespace component_testing;
namespace fdt = fuchsia_driver_test;

class FakeBootItems final : public fidl::WireServer<fuchsia_boot::Items>,
                            public component_testing::LocalComponentImpl {
 public:
  explicit FakeBootItems(std::string dtb_path, zbi_platform_id_t platform_id,
                         async_dispatcher_t* dispatcher)
      : dtb_path_(std::move(dtb_path)), platform_id_(platform_id), dispatcher_(dispatcher) {}

  // component_testing::LocalComponentImpl methods
  void OnStart() override {
    auto provider_svc = std::make_unique<vfs::Service>([this](zx::channel request,
                                                              async_dispatcher_t* dispatcher) {
      fidl::ServerEnd<fuchsia_boot::Items> server_end(std::move(request));
      bindings_.AddBinding(dispatcher_, std::move(server_end), this, fidl::kIgnoreBindingClosure);
    });

    ZX_ASSERT(outgoing()->AddPublicService(std::move(provider_svc),
                                           fidl::DiscoverableProtocolName<fuchsia_boot::Items>) ==
              ZX_OK);
  }

  // fuchsia_boot::Items methods
  void Get(GetRequestView request, GetCompleter::Sync& completer) override {
    zx::vmo vmo;
    uint32_t length = 0;
    zx_status_t status = GetBootItem(request->type, request->extra, &vmo, &length);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to get boot items", zx_status_get_string(status);
    }
    completer.Reply(std::move(vmo), length);
  }

  void Get2(Get2RequestView request, Get2Completer::Sync& completer) override {
    if (request->type == ZBI_TYPE_DEVICETREE) {
      zx::vmo vmo;
      uint32_t length = 0;
      uint32_t extra = 0;

      auto dtb = LoadDevicetreeBlob(dtb_path_.c_str());

      length = static_cast<uint32_t>(dtb.size());

      zx_status_t status = zx::vmo::create(dtb.size(), 0, &vmo);
      if (status != ZX_OK) {
        FX_LOGS(ERROR) << "vmo create failed: " << zx_status_get_string(status);
        completer.Reply(zx::error(status));
        return;
      }
      status = vmo.write(dtb.data(), 0, dtb.size());
      if (status != ZX_OK) {
        FX_LOGS(ERROR) << "vmo write failed: " << zx_status_get_string(status);
        completer.Reply(zx::error(status));
        return;
      }

      std::vector<fuchsia_boot::wire::RetrievedItems> result;
      fuchsia_boot::wire::RetrievedItems items = {std::move(vmo), length, extra};
      result.emplace_back(std::move(items));
      completer.ReplySuccess(
          fidl::VectorView<fuchsia_boot::wire::RetrievedItems>::FromExternal(result));
    } else {
      completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    }
  }

  void GetBootloaderFile(GetBootloaderFileRequestView request,
                         GetBootloaderFileCompleter::Sync& completer) override {
    completer.Reply(zx::vmo());
  }

  // Load the dtb file |name| into a vector and return it.
  static std::vector<uint8_t> LoadDevicetreeBlob(const char* name) {
    int fd = open(name, O_RDONLY);
    if (fd < 0) {
      FX_LOGS(ERROR) << "Open failed: " << strerror(errno);
      return {};
    }

    auto close_fd = fit::defer([&fd]() { close(fd); });

    struct stat stat_out;
    if (fstat(fd, &stat_out) < 0) {
      FX_LOGS(ERROR) << "fstat failed: " << strerror(errno);
      return {};
    }

    std::vector<uint8_t> vec(stat_out.st_size);
    ssize_t bytes_read = read(fd, vec.data(), stat_out.st_size);
    if (bytes_read < 0) {
      FX_LOGS(ERROR) << "read failed: " << strerror(errno);
      return {};
    }
    vec.resize(bytes_read);
    return vec;
  }

  zx_status_t GetBootItem(uint32_t type, uint32_t extra, zx::vmo* out, uint32_t* length) {
    zx::vmo vmo;
    switch (type) {
      case ZBI_TYPE_PLATFORM_ID: {
        zx_status_t status = zx::vmo::create(sizeof(zbi_platform_id_t), 0, &vmo);
        if (status != ZX_OK) {
          return status;
        }
        status = vmo.write(&platform_id_, 0, sizeof(zbi_platform_id_t));
        if (status != ZX_OK) {
          return status;
        }
        *length = sizeof(zbi_platform_id_t);
        break;
      }
      default:
        break;
    }
    *out = std::move(vmo);
    return ZX_OK;
  }

  std::string dtb_path_;
  zbi_platform_id_t platform_id_;
  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fuchsia_boot::Items> bindings_;
};

}  // namespace

void BoardTestHelper::SetupRealm() {
  auto realm_builder = component_testing::RealmBuilder::Create();
  realm_builder_ = std::make_unique<component_testing::RealmBuilder>(std::move(realm_builder));
  driver_test_realm::Setup(*realm_builder_);

  // Add FakeBootItems local component and route protocol to driver_test_realm.
  auto service_provider = std::make_unique<FakeBootItems>(dtb_path_, platform_id_, dispatcher_);

  realm_builder_->AddLocalChild(
      "dt-boot-items",
      [provider = std::move(service_provider)]() mutable { return std::move(provider); },
      component_testing::ChildOptions{});

  realm_builder_->AddRoute(
      Route{.capabilities = {Protocol{fidl::DiscoverableProtocolName<fuchsia_boot::Items>}},
            .source = ChildRef{std::string("dt-boot-items")},
            .targets = {ChildRef{"driver_test_realm"}}});

  realm_builder_->InitMutableConfigFromPackage("driver_test_realm");
  realm_builder_->SetConfigValue("driver_test_realm", "tunnel_boot_items", ConfigValue::Bool(true));
}

zx::result<> BoardTestHelper::StartRealm() {
  realm_ = std::make_unique<component_testing::RealmRoot>(realm_builder_->Build(dispatcher_));

  auto client = realm_->component().Connect<fdt::Realm>();
  if (client.is_error()) {
    FX_LOGS(ERROR) << "Failed to connect to driver test realm : " << client.status_string();
    return client.take_error();
  }

  fidl::WireSyncClient driver_test_realm{std::move(*client)};
  fidl::Arena arena;
  auto builder = fdt::wire::RealmArgs::Builder(arena);
  builder.root_driver("fuchsia-boot:///#meta/platform-bus.cm");
  builder.use_driver_framework_v2(true);

  fdt::wire::RealmArgs args = builder.Build();
  auto start_result = driver_test_realm->Start(args);
  if (start_result.status() != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to start driver test realm : " << start_result.status_string();
    return zx::error(start_result.status());
  }

  if (start_result->is_error()) {
    FX_LOGS(ERROR) << "Failed to start driver test realm : " << start_result.error();
    return start_result->take_error();
  }

  return zx::ok();
}

zx::result<> BoardTestHelper::WaitOnDevices(const std::vector<std::string>& device_paths) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto result = realm_->component().Connect("dev-topological", endpoints->server.TakeChannel());
  if (result != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to connect to dev-topological : " << zx_status_get_string(result);
    return zx::error(result);
  }

  int dev_fd;
  result = fdio_fd_create(endpoints->client.TakeChannel().release(), &dev_fd);
  if (result != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to create fd : " << zx_status_get_string(result);
    return zx::error(result);
  }

  auto close_fd = fit::defer([&dev_fd]() { close(dev_fd); });

  for (const auto& path : device_paths) {
    auto wait_result = device_watcher::RecursiveWaitForFile(dev_fd, path.c_str());
    if (wait_result.is_error()) {
      FX_LOGS(ERROR) << "Failed to wait for " << path << " : " << wait_result.status_string();
      return wait_result.take_error();
    }
  }
  return zx::ok();
}

}  // namespace fdf_devicetree::testing
