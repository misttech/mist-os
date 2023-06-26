// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/misc/drivers/compat/driver.h"

#include <dirent.h>
#include <fidl/fuchsia.boot/cpp/wire_test_base.h>
#include <fidl/fuchsia.device.fs/cpp/wire_test_base.h>
#include <fidl/fuchsia.device.manager/cpp/wire_test_base.h>
#include <fidl/fuchsia.driver.framework/cpp/wire_test_base.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <fidl/fuchsia.logger/cpp/wire_test_base.h>
#include <fidl/fuchsia.scheduler/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/testing.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <lib/sync/cpp/completion.h>

#include <unordered_set>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <mock-boot-arguments/server.h>

#include "lib/driver/testing/cpp/driver_runtime.h"
#include "sdk/lib/driver/runtime/testing/cpp/dispatcher.h"
#include "src/devices/misc/drivers/compat/v1_test.h"
#include "src/lib/storage/vfs/cpp/managed_vfs.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/service.h"

namespace fboot = fuchsia_boot;
namespace fdata = fuchsia_data;
namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace fio = fuchsia_io;
namespace flogger = fuchsia_logger;
namespace frunner = fuchsia_component_runner;

constexpr auto kOpenFlags = fio::wire::OpenFlags::kRightReadable |
                            fio::wire::OpenFlags::kRightExecutable |
                            fio::wire::OpenFlags::kNotDirectory;
constexpr auto kVmoFlags = fio::wire::VmoFlags::kRead | fio::wire::VmoFlags::kExecute;

namespace {

zx::vmo GetVmo(std::string_view path) {
  zx::result endpoints = fidl::CreateEndpoints<fio::File>();
  EXPECT_TRUE(endpoints.is_ok()) << endpoints.status_string();
  zx_status_t status = fdio_open(path.data(), static_cast<uint32_t>(kOpenFlags),
                                 endpoints->server.channel().release());
  EXPECT_EQ(status, ZX_OK) << zx_status_get_string(status);
  fidl::WireResult result = fidl::WireCall(endpoints->client)->GetBackingMemory(kVmoFlags);
  EXPECT_TRUE(result.ok()) << result.FormatDescription();
  const auto& response = result.value();
  EXPECT_TRUE(response.is_ok()) << zx_status_get_string(response.error_value());
  return std::move(response.value()->vmo);
}

class TestRootResource : public fidl::testing::WireTestBase<fboot::RootResource> {
 public:
  TestRootResource() { EXPECT_EQ(ZX_OK, zx::event::create(0, &fake_resource_)); }

  fidl::ProtocolHandler<fboot::RootResource> GetHandler() {
    return bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

 private:
  void Get(GetCompleter::Sync& completer) override {
    zx::event duplicate;
    ASSERT_EQ(ZX_OK, fake_resource_.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate));
    completer.Reply(zx::resource(duplicate.release()));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: RootResource::%s\n", name.data());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  fidl::ServerBindingGroup<fboot::RootResource> bindings_;

  // An event is similar enough that we can pretend it's the root resource, in that we can
  // send it over a FIDL channel.
  zx::event fake_resource_;
};

class TestItems : public fidl::testing::WireTestBase<fboot::Items> {
 public:
  fidl::ProtocolHandler<fboot::Items> GetHandler() {
    return bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

 private:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Items::%s\n", name.data());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::ServerBindingGroup<fboot::Items> bindings_;
};

class TestFile : public fidl::testing::WireTestBase<fio::File> {
 public:
  TestFile() = default;
  TestFile(zx_status_t status, zx::vmo vmo) : status_(status), vmo_(std::move(vmo)) {}

 private:
  void GetBackingMemory(GetBackingMemoryRequestView request,
                        GetBackingMemoryCompleter::Sync& completer) override {
    if (status_ != ZX_OK) {
      completer.ReplyError(status_);
    } else {
      completer.ReplySuccess(std::move(vmo_));
    }
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: File::%s\n", name.data());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  zx_status_t status_ = ZX_OK;
  zx::vmo vmo_;
};

class TestDirectory : public fidl::testing::WireTestBase<fio::Directory> {
 public:
  using OpenHandler = fit::function<void(OpenRequestView)>;

  void SetOpenHandler(OpenHandler open_handler) { open_handler_ = std::move(open_handler); }

 private:
  void Open(OpenRequestView request, OpenCompleter::Sync& completer) override {
    open_handler_(std::move(request));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Directory::%s\n", name.data());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  OpenHandler open_handler_;
};

class TestDevice : public fidl::WireServer<fuchsia_driver_compat::Device> {
 public:
  struct MockProtocol {
    uint64_t ctx;
    uint64_t ops;
  };

  explicit TestDevice(std::unordered_map<uint32_t, MockProtocol> banjo_protocols = {})
      : banjo_protocols_(std::move(banjo_protocols)) {}

  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override {
    completer.Reply("/dev/test/my-device");
  }

  void GetBanjoProtocol(GetBanjoProtocolRequestView request,
                        GetBanjoProtocolCompleter::Sync& completer) override {
    auto iter = banjo_protocols_.find(request->proto_id);
    if (iter == banjo_protocols_.end()) {
      completer.ReplyError(ZX_ERR_PROTOCOL_NOT_SUPPORTED);
    }
    auto& protocol = iter->second;
    completer.ReplySuccess(protocol.ops, protocol.ctx);
  }

  void GetMetadata(GetMetadataCompleter::Sync& completer) override {
    std::vector<fuchsia_driver_compat::wire::Metadata> metadata;

    std::vector<uint8_t> bytes_1 = {1, 2, 3};
    zx::vmo vmo_1;
    ASSERT_EQ(ZX_OK, zx::vmo::create(bytes_1.size(), 0, &vmo_1));
    vmo_1.write(bytes_1.data(), 0, bytes_1.size());
    size_t size = bytes_1.size();
    ASSERT_EQ(ZX_OK, vmo_1.set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size)));
    metadata.push_back(fuchsia_driver_compat::wire::Metadata{.type = 1, .data = std::move(vmo_1)});

    std::vector<uint8_t> bytes_2 = {4, 5, 6};
    zx::vmo vmo_2;
    ASSERT_EQ(ZX_OK, zx::vmo::create(bytes_1.size(), 0, &vmo_2));
    vmo_2.write(bytes_2.data(), 0, bytes_2.size());
    ASSERT_EQ(ZX_OK, vmo_2.set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size)));
    metadata.push_back(fuchsia_driver_compat::wire::Metadata{.type = 2, .data = std::move(vmo_2)});

    completer.ReplySuccess(fidl::VectorView<fuchsia_driver_compat::wire::Metadata>::FromExternal(
        metadata.data(), metadata.size()));
  }

 private:
  std::unordered_map<uint32_t, MockProtocol> banjo_protocols_;
};

class TestProfileProvider : public fidl::testing::WireTestBase<fuchsia_scheduler::ProfileProvider> {
 public:
  TestProfileProvider() = default;

  explicit TestProfileProvider(std::string expected_role)
      : expected_role_(std::move(expected_role)) {}

  fidl::ProtocolHandler<fuchsia_scheduler::ProfileProvider> GetHandler() {
    return bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

 private:
  void GetProfile(GetProfileRequestView request, GetProfileCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, zx::profile());
  }

  void GetDeadlineProfile(GetDeadlineProfileRequestView request,
                          GetDeadlineProfileCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, zx::profile());
  }

  void SetProfileByRole(SetProfileByRoleRequestView request,
                        SetProfileByRoleCompleter::Sync& completer) override {
    if (!request->handle.is_valid()) {
      completer.Reply(ZX_ERR_INVALID_ARGS);
      return;
    }
    if (request->role.get() != expected_role_) {
      completer.Reply(ZX_ERR_BAD_PATH);
      return;
    }
    completer.Reply(ZX_OK);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: ProfileProvider::%s", name.data());
  }

  fidl::ServerBindingGroup<fuchsia_scheduler::ProfileProvider> bindings_;
  std::string expected_role_;
};

class TestSystemStateTransition
    : public fidl::testing::WireTestBase<fuchsia_device_manager::SystemStateTransition> {
 public:
  fidl::ProtocolHandler<fuchsia_device_manager::SystemStateTransition> GetHandler() {
    return bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

 private:
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override {
    completer.Reply(fuchsia_device_manager::wire::SystemPowerState::kFullyOn);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: SystemStateTransition::%s", name.data());
  }

  fidl::ServerBindingGroup<fuchsia_device_manager::SystemStateTransition> bindings_;
};

class TestLogSink : public fidl::testing::WireTestBase<flogger::LogSink> {
 public:
  TestLogSink() = default;

  ~TestLogSink() {
    if (completer_) {
      completer_->ReplySuccess({});
    }
  }

 private:
  void ConnectStructured(ConnectStructuredRequestView request,
                         ConnectStructuredCompleter::Sync& completer) override {
    socket_ = std::move(request->socket);
  }
  void WaitForInterestChange(WaitForInterestChangeCompleter::Sync& completer) override {
    if (first_call_) {
      first_call_ = false;
      completer.ReplySuccess({});
    } else {
      completer_ = completer.ToAsync();
    }
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: LogSink::%s\n", name.data());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  zx::socket socket_;
  bool first_call_ = true;
  std::optional<WaitForInterestChangeCompleter::Async> completer_;
};

class IncomingNamespace {
 public:
  zx::result<> Start(std::string_view v1_driver_path, zx_status_t compat_file_response,
                     std::unordered_map<std::string, TestDevice> devices,
                     std::string expected_profile_role, fidl::ServerEnd<fio::Directory> pkg_server,
                     fidl::ServerEnd<fio::Directory> svc_server) {
    async_dispatcher_t* dispatcher = async_get_default_dispatcher();
    profile_provider_.emplace(std::move(expected_profile_role));

    std::map<std::string, std::string> arguments;
    arguments["kernel.shell"] = "true";
    arguments["driver.foo"] = "true";
    arguments["clock.backstop"] = "0";
    boot_args_ = mock_boot_arguments::Server(std::move(arguments));

    // Setup and bind "/pkg" directory.
    compat_file_ = TestFile(compat_file_response, GetVmo("/pkg/driver/compat.so"));
    v1_test_file_ = TestFile(ZX_OK, GetVmo(v1_driver_path));
    firmware_file_ = TestFile(ZX_OK, GetVmo("/pkg/lib/firmware/test"));
    pkg_directory_.SetOpenHandler([this, dispatcher](TestDirectory::OpenRequestView request) {
      fidl::ServerEnd<fio::File> server_end(request->object.TakeChannel());
      if (request->path.get() == "driver/compat.so") {
        fidl::BindServer(dispatcher, std::move(server_end), &compat_file_);
      } else if (request->path.get() == "driver/v1_test.so") {
        fidl::BindServer(dispatcher, std::move(server_end), &v1_test_file_);
      } else if (request->path.get() == "lib/firmware/test") {
        fidl::BindServer(dispatcher, std::move(server_end), &firmware_file_);
      } else {
        FAIL() << "Unexpected file: " << request->path.get();
      }
    });
    fidl::BindServer(dispatcher, std::move(pkg_server), &pkg_directory_);

    // Setup and bind "/svc" directory.
    {
      zx::result result = outgoing.AddUnmanagedProtocol<flogger::LogSink>(
          [dispatcher](fidl::ServerEnd<flogger::LogSink> server) {
            fidl::BindServer(dispatcher, std::move(server), std::make_unique<TestLogSink>());
          });
      if (result.is_error()) {
        return result.take_error();
      }

      result = outgoing.AddUnmanagedProtocol<fboot::RootResource>(root_resource_.GetHandler());
      if (result.is_error()) {
        return result.take_error();
      }

      result = outgoing.AddUnmanagedProtocol<fboot::Items>(items_.GetHandler());
      if (result.is_error()) {
        return result.take_error();
      }

      result = outgoing.AddUnmanagedProtocol<fboot::Arguments>(
          [this, dispatcher](fidl::ServerEnd<fboot::Arguments> server) {
            fidl::BindServer(dispatcher, std::move(server), &boot_args_);
          });
      if (result.is_error()) {
        return result.take_error();
      }

      result = outgoing.AddUnmanagedProtocol<fuchsia_scheduler::ProfileProvider>(
          profile_provider_->GetHandler());
      if (result.is_error()) {
        return result.take_error();
      }

      result = outgoing.AddUnmanagedProtocol<fuchsia_device_manager::SystemStateTransition>(
          system_state_transition_.GetHandler());
      if (result.is_error()) {
        return result.take_error();
      }

      devices_ = std::move(devices);
      for (auto& device : devices_) {
        TestDevice* device_ptr = &device.second;
        zx::result result = outgoing.AddService<fuchsia_driver_compat::Service>(
            fuchsia_driver_compat::Service::InstanceHandler({
                .device =
                    [device_ptr,
                     dispatcher](fidl::ServerEnd<fuchsia_driver_compat::Device> server) {
                      fidl::BindServer(dispatcher, std::move(server), device_ptr);
                    },
            }),
            device.first);
        if (result.is_error()) {
          return result.take_error();
        }
      }

      zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
      if (endpoints.is_error()) {
        return endpoints.take_error();
      }
      if (zx::result result = outgoing.Serve(std::move(endpoints->server)); result.is_error()) {
        return result.take_error();
      }
      fidl::OneWayError error =
          fidl::WireCall(endpoints->client)
              ->Open(fuchsia_io::wire::OpenFlags::kDirectory, fuchsia_io::ModeType(), "svc",
                     fidl::ServerEnd<fuchsia_io::Node>(svc_server.TakeChannel()));
      if (!error.ok()) {
        return zx::error(error.status());
      }
    }

    return zx::ok();
  }

 private:
  std::unordered_map<std::string, TestDevice> devices_;
  TestRootResource root_resource_;
  std::optional<TestProfileProvider> profile_provider_;
  mock_boot_arguments::Server boot_args_;
  TestItems items_;
  TestFile compat_file_;
  TestFile v1_test_file_;
  TestFile firmware_file_;
  TestDirectory pkg_directory_;
  TestSystemStateTransition system_state_transition_;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

// Log through a zx_driver logger.
void log(zx_driver_t* drv, FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
         const char* msg, ...) {
  va_list args;
  va_start(args, msg);
  drv->Log(severity, tag, file, line, msg, args);
  va_end(args);
}

}  // namespace

class DriverTest : public testing::Test {
 protected:
  fdf_testing::TestNode& node() { return node_.value(); }

  void SetUp() override {
    ns_loop_.StartThread("fidl-server-thread");
    node_.emplace("root", dispatcher());
  }

  void TearDown() override {}

  struct StartDriverArgs {
    std::string_view v1_driver_path;
    zx_protocol_device_t ops = {};
    std::unordered_map<std::string, TestDevice> devices = {{"default", TestDevice()}};
    zx_status_t expected_driver_status = ZX_OK;
    zx_status_t compat_file_response = ZX_OK;
    std::string expected_profile_role;
  };
  std::unique_ptr<compat::Driver> StartDriver(StartDriverArgs args) {
    auto outgoing_dir_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(outgoing_dir_endpoints.is_ok());
    auto pkg_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(pkg_endpoints.is_ok());
    auto svc_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(svc_endpoints.is_ok());

    // Setup the node.
    zx::result node_client = node_->CreateNodeChannel();
    EXPECT_EQ(ZX_OK, node_client.status_value());

    zx::result ns_start_result =
        incoming_ns_.SyncCall(&IncomingNamespace::Start, args.v1_driver_path,
                              args.compat_file_response, args.devices, args.expected_profile_role,
                              std::move(pkg_endpoints->server), std::move(svc_endpoints->server));
    EXPECT_EQ(ZX_OK, ns_start_result.status_value());

    auto entry_pkg = frunner::ComponentNamespaceEntry(
        {.path = std::string("/pkg"), .directory = std::move(pkg_endpoints->client)});
    auto entry_svc = frunner::ComponentNamespaceEntry(
        {.path = std::string("/svc"), .directory = std::move(svc_endpoints->client)});
    std::vector<frunner::ComponentNamespaceEntry> ns_entries;
    ns_entries.push_back(std::move(entry_pkg));
    ns_entries.push_back(std::move(entry_svc));

    device_ops_ = args.ops;
    std::vector<fdf::NodeSymbol> symbols({fdf::NodeSymbol(
        {.name = compat::kOps, .address = reinterpret_cast<uint64_t>(&device_ops_)})});

    auto program_entry =
        fdata::DictionaryEntry("compat", std::make_unique<fdata::DictionaryValue>(
                                             fdata::DictionaryValue::WithStr("driver/v1_test.so")));
    std::vector<fdata::DictionaryEntry> program_vec;
    program_vec.push_back(std::move(program_entry));
    fdata::Dictionary program({.entries = std::move(program_vec)});

    fdf::DriverStartArgs start_args({
        .node = std::move(node_client.value()),
        .symbols = std::move(symbols),
        .url = std::string("fuchsia-pkg://fuchsia.com/driver#meta/driver.cm"),
        .program = std::move(program),
        .incoming = std::move(ns_entries),
        .outgoing_dir = std::move(outgoing_dir_endpoints->server),
        .config = std::nullopt,
        .node_name = "node",
    });

    // Start driver.
    struct Context {
      zx_status_t* status;
      void** driver;
    };
    void* driver = nullptr;
    zx_status_t status = ZX_ERR_INTERNAL;
    Context ctx = {
        .status = &status,
        .driver = &driver,
    };
    fdf::StartCompleter start_completer(
        [](void* context, zx_status_t status, void* driver) {
          auto* ctx = static_cast<Context*>(context);
          *ctx->status = status;
          *ctx->driver = driver;
        },
        &ctx);

    compat::DriverFactory::CreateDriver(
        std::move(start_args),
        fdf::UnownedSynchronizedDispatcher(fdf::Dispatcher::GetCurrent()->get()),
        std::move(start_completer));

    while (driver == nullptr) {
      fdf_testing_run_until_idle();
    };
    EXPECT_EQ(status, args.expected_driver_status);
    if (status != ZX_OK) {
      EXPECT_NE(driver, nullptr);
    }

    return std::unique_ptr<compat::Driver>(static_cast<compat::Driver*>(driver));
  }

  void UnbindAndFreeDriver(std::unique_ptr<compat::Driver> driver) {
    libsync::Completion completion;

    fdf::PrepareStopCompleter completer(
        [](void* cookie, zx_status_t status) {
          static_cast<libsync::Completion*>(cookie)->Signal();
        },
        &completion);
    driver->PrepareStop(std::move(completer));

    // Keep running the test loop while we're waiting for a signal on the dispatcher thread.
    // The dispatcher thread needs to interact with our Node servers, which run on the test loop.
    while (!completion.signaled()) {
      fdf_testing_run_until_idle();
    }

    driver.reset();
  }

  void WaitForChildDeviceAdded() {
    while (node().children().empty()) {
      fdf_testing_run_until_idle();
    }
    EXPECT_FALSE(node().children().empty());
  }

  async_dispatcher_t* dispatcher() { return fdf::Dispatcher::GetCurrent()->async_dispatcher(); }

 private:
  async::Loop ns_loop_ = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_ns_{ns_loop_.dispatcher(),
                                                                      std::in_place};
  zx_protocol_device_t device_ops_;
  fdf_testing::DriverRuntime runtime_;
  std::optional<fdf_testing::TestNode> node_;
};

class GlobalLoggerListTest : public testing::Test {
 protected:
  std::shared_ptr<fdf::Logger> NewLogger(const std::string& name) {
    auto svc = fidl::CreateEndpoints<fio::Directory>();
    ZX_ASSERT(ZX_OK == svc.status_value());

    fidl::Arena arena;
    fidl::VectorView<frunner::wire::ComponentNamespaceEntry> entries(arena, 1);
    entries[0] = frunner::wire::ComponentNamespaceEntry::Builder(arena)
                     .path("/svc")
                     .directory(std::move(svc->client))
                     .Build();
    auto ns = fdf::Namespace::Create(entries);
    ZX_ASSERT(ZX_OK == ns.status_value());

    auto logger = fdf::Logger::Create(*ns, dispatcher(), name, FUCHSIA_LOG_INFO, false);
    ZX_ASSERT(ZX_OK == logger.status_value());
    return std::shared_ptr<fdf::Logger>((*logger).release());
  }
  async_dispatcher_t* dispatcher() { return fdf::Dispatcher::GetCurrent()->async_dispatcher(); }

 private:
  fdf_testing::DriverRuntime runtime_;
};

TEST_F(DriverTest, Start) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_test.so",
      .ops =
          {
              .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
          },
  });

  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  // Verify that v1_test.so has set a context.
  std::unique_ptr<V1Test> v1_test(static_cast<V1Test*>(driver->Context()));
  ASSERT_NE(nullptr, v1_test.get());

  // Verify v1_test.so state after bind.
  EXPECT_TRUE(v1_test->did_bind);
  EXPECT_EQ(ZX_OK, v1_test->status);
  EXPECT_FALSE(v1_test->did_create);
  EXPECT_FALSE(v1_test->did_release);

  // Verify v1_test.so state after release.
  UnbindAndFreeDriver(std::move(driver));
  EXPECT_TRUE(v1_test->did_release);
}

TEST_F(DriverTest, Start_WithCreate) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_create_test.so",
  });

  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  // Verify that v1_test.so has set a context.
  std::unique_ptr<V1Test> v1_test(static_cast<V1Test*>(driver->Context()));
  ASSERT_NE(nullptr, v1_test.get());

  // Verify v1_test.so state after bind.
  EXPECT_EQ(ZX_OK, v1_test->status);
  EXPECT_FALSE(v1_test->did_bind);
  EXPECT_TRUE(v1_test->did_create);
  EXPECT_FALSE(v1_test->did_release);

  // Verify v1_test.so state after release.
  UnbindAndFreeDriver(std::move(driver));
  EXPECT_TRUE(v1_test->did_release);
}

TEST_F(DriverTest, Start_MissingBindAndCreate) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_missing_test.so",
      .expected_driver_status = ZX_ERR_BAD_STATE,
  });
  EXPECT_TRUE(node().children().empty());

  // Verify that v1_test.so has not set a context.
  EXPECT_EQ(nullptr, driver->Context());
}

TEST_F(DriverTest, Start_DeviceAddNull) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_device_add_null_test.so",
  });

  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, Start_CheckCompatService) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_device_add_null_test.so",
  });

  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  // Check topological path.
  ASSERT_STREQ(driver->GetDevice().topological_path().data(), "/dev/test/my-device");

  // Check metadata.
  std::array<uint8_t, 3> expected_metadata;
  std::array<uint8_t, 3> metadata;
  size_t size = 0;

  ASSERT_EQ(driver->GetDevice().GetMetadata(1, metadata.data(), metadata.size(), &size), ZX_OK);
  ASSERT_EQ(size, 3ul);
  expected_metadata = {1, 2, 3};
  ASSERT_EQ(metadata, expected_metadata);

  ASSERT_EQ(driver->GetDevice().GetMetadata(2, metadata.data(), metadata.size(), &size), ZX_OK);
  ASSERT_EQ(size, 3ul);
  expected_metadata = {4, 5, 6};
  ASSERT_EQ(metadata, expected_metadata);

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, DISABLED_Start_RootResourceIsConstant) {
  // Set the root resource before the test starts.
  zx_handle_t resource;
  {
    std::scoped_lock lock(kDriverGlobalsLock);
    ASSERT_EQ(ZX_OK, zx_event_create(0, kRootResource.reset_and_get_address()));
    resource = kRootResource.get();
  }

  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_device_add_null_test.so",
  });
  zx_handle_t resource2 = get_root_resource();

  // Check that the root resource's value did not change.
  ASSERT_EQ(resource, resource2);
}

TEST_F(DriverTest, Start_GetBackingMemory) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_test.so",
      .expected_driver_status = ZX_ERR_UNAVAILABLE,
      .compat_file_response = ZX_ERR_UNAVAILABLE,
  });

  // Verify that v1_test.so has not added a child device.
  EXPECT_TRUE(node().children().empty());

  // Verify that v1_test.so has not set a context.
  EXPECT_EQ(nullptr, driver->Context());

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, Start_BindFailed) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_test.so",
      .expected_driver_status = ZX_ERR_NOT_SUPPORTED,
  });

  // Verify that v1_test.so has set a context.
  while (!driver->Context()) {
    fdf_testing_run_until_idle();
  }
  std::unique_ptr<V1Test> v1_test(static_cast<V1Test*>(driver->Context()));
  ASSERT_NE(nullptr, v1_test.get());

  // Verify that v1_test.so has been bound.
  while (!v1_test->did_bind) {
    fdf_testing_run_until_idle();
  }

  // Verify that v1_test.so has not added a child device.
  EXPECT_TRUE(node().children().empty());

  EXPECT_TRUE(v1_test->did_bind);
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, v1_test->status);

  EXPECT_FALSE(v1_test->did_create);
  EXPECT_FALSE(v1_test->did_release);

  // Verify v1_test.so state after release.
  UnbindAndFreeDriver(std::move(driver));

  EXPECT_TRUE(v1_test->did_release);
}

TEST_F(DriverTest, GetVariable) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_test.so",
      .ops =
          {
              .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
          },
  });
  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  // Verify that v1_test.so has set a context.
  std::unique_ptr<V1Test> v1_test(static_cast<V1Test*>(driver->Context()));
  ASSERT_NE(nullptr, v1_test.get());

  char variable[20];
  size_t actual;
  ASSERT_EQ(ZX_OK,
            device_get_variable(v1_test->zxdev, "driver.foo", variable, sizeof(variable), &actual));
  ASSERT_EQ(actual, 4u);
  ASSERT_EQ(strncmp(variable, "true", sizeof(variable)), 0);
  ASSERT_EQ(ZX_OK, device_get_variable(v1_test->zxdev, "clock.backstop", variable, sizeof(variable),
                                       &actual));
  ASSERT_EQ(actual, 1u);
  ASSERT_EQ(strncmp(variable, "0", sizeof(variable)), 0);
  // Invalid variable name
  ASSERT_EQ(ZX_ERR_NOT_FOUND, device_get_variable(v1_test->zxdev, "kernel.shell", variable,
                                                  sizeof(variable), &actual));
  // Buffer too small
  ASSERT_EQ(ZX_ERR_BUFFER_TOO_SMALL,
            device_get_variable(v1_test->zxdev, "driver.foo", variable, 1, &actual));
  ASSERT_EQ(actual, 4u);

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, SetProfileByRole) {
  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_test.so",
      .ops =
          {
              .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
          },
      .expected_profile_role = "test-profile",
  });
  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  // Verify that v1_test.so has set a context.
  std::unique_ptr<V1Test> v1_test(static_cast<V1Test*>(driver->Context()));
  ASSERT_NE(nullptr, v1_test.get());

  constexpr char kThreadName[] = "test-thread";
  zx::thread thread;
  ASSERT_EQ(ZX_OK,
            zx::thread::create(*zx::process::self(), kThreadName, sizeof(kThreadName), 0, &thread));
  ASSERT_EQ(ZX_OK, device_set_profile_by_role(v1_test->zxdev, thread.release(), "test-profile",
                                              strlen("test-profile")));

  ASSERT_EQ(ZX_OK,
            zx::thread::create(*zx::process::self(), kThreadName, sizeof(kThreadName), 0, &thread));
  ASSERT_EQ(ZX_ERR_BAD_PATH, device_set_profile_by_role(v1_test->zxdev, thread.release(),
                                                        "bad-role", strlen("bad-role")));

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, GetFragmentProtocol) {
  const char* kFragmentName = "fragment-name";
  const uint32_t kFragmentProtoId = ZX_PROTOCOL_BLOCK;
  const uint64_t kFragmentOps = 0x1234;
  const uint64_t kFragmentCtx = 0x4567;

  auto driver = StartDriver({
      .v1_driver_path = "/pkg/driver/v1_test.so",
      .ops =
          {
              .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
          },
      .devices =
          {
              {kFragmentName, TestDevice({
                                  {kFragmentProtoId, {kFragmentCtx, kFragmentOps}},
                              })},
          },
  });

  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  // Verify that v1_test.so has set a context.
  std::unique_ptr<V1Test> v1_test(static_cast<V1Test*>(driver->Context()));
  ASSERT_NE(nullptr, v1_test.get());

  struct GenericProtocol {
    const void* ops;
    void* ctx;
  } proto;
  ASSERT_EQ(ZX_OK, driver->GetFragmentProtocol(kFragmentName, kFragmentProtoId, &proto));
  ASSERT_EQ(reinterpret_cast<uint64_t>(proto.ops), kFragmentOps);
  ASSERT_EQ(reinterpret_cast<uint64_t>(proto.ctx), kFragmentCtx);
  ASSERT_EQ(ZX_ERR_NOT_FOUND,
            driver->GetFragmentProtocol("unknown-fragment", kFragmentProtoId, &proto));

  // Verify v1_test.so state after release.
  UnbindAndFreeDriver(std::move(driver));
  EXPECT_TRUE(v1_test->did_release);
}

TEST_F(GlobalLoggerListTest, TestWithoutNodeNames) {
  compat::GlobalLoggerList global_list(false);
  ASSERT_EQ(std::nullopt, global_list.loggers_count_for_testing("path_1"));

  auto logger_1 = NewLogger("logger_1");
  auto* zx_driver_1 = global_list.AddLogger("path_1", logger_1, std::nullopt);
  ASSERT_EQ(1, global_list.loggers_count_for_testing("path_1"));
  auto& node_names_res = zx_driver_1->node_names_for_testing();
  ASSERT_EQ(0ul, node_names_res.size());

  auto logger_2 = NewLogger("logger_2");
  auto* zx_driver_2 = global_list.AddLogger("path_1", logger_2, std::nullopt);
  ASSERT_EQ(2, global_list.loggers_count_for_testing("path_1"));
  ASSERT_EQ(zx_driver_1, zx_driver_2);
  auto& node_names_res_2 = zx_driver_2->node_names_for_testing();
  ASSERT_EQ(&node_names_res, &node_names_res_2);
  ASSERT_EQ(0ul, node_names_res_2.size());

  auto logger_3 = NewLogger("logger_3");
  auto* zx_driver_3 = global_list.AddLogger("path_2", logger_3, std::nullopt);
  ASSERT_EQ(1, global_list.loggers_count_for_testing("path_2"));
  ASSERT_NE(zx_driver_3, zx_driver_2);
  auto& node_names_res_3 = zx_driver_3->node_names_for_testing();
  ASSERT_NE(&node_names_res_2, &node_names_res_3);
  ASSERT_EQ(0ul, node_names_res_3.size());
  log(zx_driver_3, FUCHSIA_LOG_INFO, nullptr, __FILE__, __LINE__, "Hello!");

  global_list.RemoveLogger("path_2", logger_3, std::nullopt);
  ASSERT_EQ(0, global_list.loggers_count_for_testing("path_2"));

  global_list.RemoveLogger("path_1", logger_1, std::nullopt);
  ASSERT_EQ(1, global_list.loggers_count_for_testing("path_1"));
  ASSERT_EQ(0ul, node_names_res_2.size());

  global_list.RemoveLogger("path_1", logger_2, std::nullopt);
  ASSERT_EQ(0, global_list.loggers_count_for_testing("path_1"));

  // Make sure we can still log with the zx_drivers that we got even when it is emptied out.
  log(zx_driver_3, FUCHSIA_LOG_INFO, nullptr, __FILE__, __LINE__, "Done with test: %s",
      "TestWithoutNodeNames");
}

TEST_F(GlobalLoggerListTest, TestWithNodeNames) {
  compat::GlobalLoggerList global_list(true);
  ASSERT_EQ(std::nullopt, global_list.loggers_count_for_testing("path_1"));

  auto logger_1 = NewLogger("logger_1");
  auto* zx_driver_1 = global_list.AddLogger("path_1", logger_1, "node_1");
  ASSERT_EQ(1, global_list.loggers_count_for_testing("path_1"));
  auto& node_names_res = zx_driver_1->node_names_for_testing();
  ASSERT_EQ(1ul, node_names_res.size());
  ASSERT_EQ("node_1", node_names_res[0]);

  auto logger_2 = NewLogger("logger_2");
  auto* zx_driver_2 = global_list.AddLogger("path_1", logger_2, "node_2");
  ASSERT_EQ(2, global_list.loggers_count_for_testing("path_1"));
  ASSERT_EQ(zx_driver_1, zx_driver_2);
  auto& node_names_res_2 = zx_driver_2->node_names_for_testing();
  ASSERT_EQ(&node_names_res, &node_names_res_2);
  ASSERT_EQ(2ul, node_names_res_2.size());
  ASSERT_EQ("node_1", node_names_res_2[0]);
  ASSERT_EQ("node_2", node_names_res_2[1]);

  auto logger_3 = NewLogger("logger_3");
  auto* zx_driver_3 = global_list.AddLogger("path_2", logger_3, "node_3");
  ASSERT_EQ(1, global_list.loggers_count_for_testing("path_2"));
  ASSERT_NE(zx_driver_3, zx_driver_2);
  auto& node_names_res_3 = zx_driver_3->node_names_for_testing();
  ASSERT_NE(&node_names_res_2, &node_names_res_3);
  ASSERT_EQ(1ul, node_names_res_3.size());
  ASSERT_EQ("node_3", node_names_res_3[0]);
  log(zx_driver_3, FUCHSIA_LOG_INFO, nullptr, __FILE__, __LINE__, "Hello!");

  global_list.RemoveLogger("path_2", logger_3, "node_3");
  ASSERT_EQ(0, global_list.loggers_count_for_testing("path_2"));

  global_list.RemoveLogger("path_1", logger_1, "node_1");
  ASSERT_EQ(1, global_list.loggers_count_for_testing("path_1"));
  ASSERT_EQ(1ul, node_names_res_2.size());
  ASSERT_EQ("node_2", node_names_res_2[0]);

  global_list.RemoveLogger("path_1", logger_2, "node_2");
  ASSERT_EQ(0, global_list.loggers_count_for_testing("path_1"));

  // Make sure we can still log with the zx_drivers that we got even when it is emptied out.
  log(zx_driver_3, FUCHSIA_LOG_INFO, nullptr, __FILE__, __LINE__, "Done with test: %s",
      "TestWithNodeNames");
}
