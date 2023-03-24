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

#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"
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

  // An event is similar enough that we can pretend it's the root resource, in that we can
  // send it over a FIDL channel.
  zx::event fake_resource_;
};

class TestItems : public fidl::testing::WireTestBase<fboot::Items> {
 private:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Items::%s\n", name.data());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
};

class TestFile : public fidl::testing::WireTestBase<fio::File> {
 public:
  void SetStatus(zx_status_t status) { status_ = status; }
  void SetVmo(zx::vmo vmo) { vmo_ = std::move(vmo); }

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
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override {
    completer.Reply("/dev/test/my-device");
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
};

class TestProfileProvider : public fidl::testing::WireTestBase<fuchsia_scheduler::ProfileProvider> {
 public:
  void GetProfile(GetProfileRequestView request, GetProfileCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, zx::profile());
  }

  void GetDeadlineProfile(GetDeadlineProfileRequestView request,
                          GetDeadlineProfileCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, zx::profile());
  }

  void SetProfileByRole(SetProfileByRoleRequestView request,
                        SetProfileByRoleCompleter::Sync& completer) override {
    if (set_profile_by_role_callback_) {
      set_profile_by_role_callback_(request);
    }
    completer.Reply(ZX_OK);
  }
  void SetSetProfileByRoleCallback(std::function<void(SetProfileByRoleRequestView&)> cb) {
    set_profile_by_role_callback_ = std::move(cb);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: ProfileProvider::%s", name.data());
  }

 private:
  std::function<void(SetProfileByRoleRequestView&)> set_profile_by_role_callback_;
};

class TestSystemStateTransition
    : public fidl::testing::WireTestBase<fuchsia_device_manager::SystemStateTransition> {
 public:
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override {
    completer.Reply(fuchsia_device_manager::wire::SystemPowerState::kFullyOn);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: SystemStateTransition::%s", name.data());
  }
};

class TestExporter : public fidl::testing::WireTestBase<fuchsia_device_fs::Exporter> {
 public:
  void Export(ExportRequestView request, ExportCompleter::Sync& completer) override {
    auto [i, inserted] = devfs_paths_.emplace(request->topological_path.get());
    if (!inserted) {
      std::cerr << "Cannot export \"" << request->topological_path.get()
                << "\": Path already exists";
      completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    }

    connectors_.push_back(std::move(request->open_client));

    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: TestExporter::%s", name.data());
  }

  void AssertDevfsPaths(std::unordered_set<std::string> expected) const {
    ASSERT_EQ(devfs_paths_, expected);
  }

 private:
  // We don't do anything with these clients but we hold on to them to keep them alive.
  std::vector<fidl::ClientEnd<fuchsia_device_fs::Connector>> connectors_;
  std::unordered_set<std::string> devfs_paths_;
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

}  // namespace

class DriverTest : public testing::Test {
 protected:
  fdf_testing::TestNode& node() { return node_.value(); }
  TestFile& compat_file() { return compat_file_; }

  void SetUp() override {
    ASSERT_EQ(ZX_OK, driver_dispatcher_.StartAsDefault({}, "driver-test-loop").status_value());

    fidl_loop_.StartThread("fidl-server-thread");

    std::map<std::string, std::string> arguments;
    arguments["kernel.shell"] = "true";
    arguments["driver.foo"] = "true";
    arguments["clock.backstop"] = "0";
    boot_args_ = mock_boot_arguments::Server(std::move(arguments));

    node_.emplace("root", dispatcher());
  }

  void TearDown() override {
    bool did_shutdown = false;
    vfs_->Shutdown([&did_shutdown](auto status) { did_shutdown = true; });
    while (!did_shutdown) {
      fdf_testing_run_until_idle();
    }

    ASSERT_TRUE(did_shutdown);
  }

  std::unique_ptr<compat::Driver> StartDriver(std::string_view v1_driver_path,
                                              const zx_protocol_device_t* ops) {
    auto outgoing_dir_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(outgoing_dir_endpoints.is_ok());
    auto pkg_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(pkg_endpoints.is_ok());
    auto svc_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(svc_endpoints.is_ok());
    auto compat_service_endpoints = fidl::CreateEndpoints<fio::Directory>();
    EXPECT_TRUE(compat_service_endpoints.is_ok());

    // Setup the node.
    zx::result node_client = node_->CreateNodeChannel();
    EXPECT_EQ(ZX_OK, node_client.status_value());

    // Setup and bind "/pkg" directory.
    compat_file_.SetVmo(GetVmo("/pkg/driver/compat.so"));
    v1_test_file_.SetVmo(GetVmo(v1_driver_path));
    firmware_file_.SetVmo(GetVmo("/pkg/lib/firmware/test"));
    pkg_directory_.SetOpenHandler([this](TestDirectory::OpenRequestView request) {
      fidl::ServerEnd<fio::File> server_end(request->object.TakeChannel());
      if (request->path.get() == "driver/compat.so") {
        fidl::BindServer(dispatcher(), std::move(server_end), &compat_file_);
      } else if (request->path.get() == "driver/v1_test.so") {
        fidl::BindServer(dispatcher(), std::move(server_end), &v1_test_file_);
      } else if (request->path.get() == "lib/firmware/test") {
        fidl::BindServer(dispatcher(), std::move(server_end), &firmware_file_);
      } else {
        FAIL() << "Unexpected file: " << request->path.get();
      }
    });
    fidl::BindServer(dispatcher(), std::move(pkg_endpoints->server), &pkg_directory_);

    // Setup and bind "/svc" directory.
    {
      auto svc = fbl::MakeRefCounted<fs::PseudoDir>();
      svc->AddEntry(fidl::DiscoverableProtocolName<flogger::LogSink>,
                    fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
                      fidl::ServerEnd<flogger::LogSink> server_end(std::move(server));
                      fidl::BindServer(fidl_loop_.dispatcher(), std::move(server_end),
                                       std::make_unique<TestLogSink>());
                      return ZX_OK;
                    }));

      svc->AddEntry(fidl::DiscoverableProtocolName<fboot::RootResource>,
                    fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
                      fidl::ServerEnd<fboot::RootResource> server_end(std::move(server));
                      fidl::BindServer(dispatcher(), std::move(server_end), &root_resource_);
                      return ZX_OK;
                    }));

      svc->AddEntry(fidl::DiscoverableProtocolName<fboot::Items>,
                    fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
                      fidl::ServerEnd<fboot::Items> server_end(std::move(server));
                      fidl::BindServer(dispatcher(), std::move(server_end), &items_);
                      return ZX_OK;
                    }));

      svc->AddEntry(fidl::DiscoverableProtocolName<fboot::Arguments>,
                    fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
                      fidl::ServerEnd<fboot::Arguments> server_end(std::move(server));
                      fidl::BindServer(fidl_loop_.dispatcher(), std::move(server_end), &boot_args_);
                      return ZX_OK;
                    }));

      svc->AddEntry(fidl::DiscoverableProtocolName<fuchsia_device_fs::Exporter>,
                    fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
                      fidl::ServerEnd<fuchsia_device_fs::Exporter> server_end(std::move(server));
                      fidl::BindServer(fidl_loop_.dispatcher(), std::move(server_end), &exporter_);
                      return ZX_OK;
                    }));

      svc->AddEntry(
          fidl::DiscoverableProtocolName<fuchsia_scheduler::ProfileProvider>,
          fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
            fidl::ServerEnd<fuchsia_scheduler::ProfileProvider> server_end(std::move(server));
            fidl::BindServer(fidl_loop_.dispatcher(), std::move(server_end), &profile_provider_);
            return ZX_OK;
          }));

      svc->AddEntry(fidl::DiscoverableProtocolName<fuchsia_device_manager::SystemStateTransition>,
                    fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
                      fidl::ServerEnd<fuchsia_device_manager::SystemStateTransition> server_end(
                          std::move(server));
                      fidl::BindServer(fidl_loop_.dispatcher(), std::move(server_end),
                                       &system_state_transition_);
                      return ZX_OK;
                    }));

      auto compat_dir = fbl::MakeRefCounted<fs::PseudoDir>();
      auto compat_default_dir = fbl::MakeRefCounted<fs::PseudoDir>();
      compat_default_dir->AddEntry(
          "device", fbl::MakeRefCounted<fs::Service>([this](zx::channel server) {
            fidl::ServerEnd<fuchsia_driver_compat::Device> server_end(std::move(server));
            fidl::BindServer(dispatcher(), std::move(server_end), &test_device_);
            return ZX_OK;
          }));
      compat_dir->AddEntry("default", compat_default_dir);
      svc->AddEntry("fuchsia.driver.compat.Service", compat_dir);

      vfs_.emplace(fidl_loop_.dispatcher());
      vfs_->ServeDirectory(svc, std::move(svc_endpoints->server));
    }

    auto entry_pkg = frunner::ComponentNamespaceEntry(
        {.path = std::string("/pkg"), .directory = std::move(pkg_endpoints->client)});
    auto entry_svc = frunner::ComponentNamespaceEntry(
        {.path = std::string("/svc"), .directory = std::move(svc_endpoints->client)});
    std::vector<frunner::ComponentNamespaceEntry> ns_entries;
    ns_entries.push_back(std::move(entry_pkg));
    ns_entries.push_back(std::move(entry_svc));

    std::vector<fdf::NodeSymbol> symbols(
        {fdf::NodeSymbol({.name = compat::kOps, .address = reinterpret_cast<uint64_t>(ops)})});

    auto program_entry =
        fdata::DictionaryEntry("compat", std::make_unique<fdata::DictionaryValue>(
                                             fdata::DictionaryValue::WithStr("driver/v1_test.so")));
    std::vector<fdata::DictionaryEntry> program_vec;
    program_vec.push_back(std::move(program_entry));
    fdata::Dictionary program({.entries = std::move(program_vec)});

    fdf::DriverStartArgs start_args(
        {.node = std::move(node_client.value()),
         .symbols = std::move(symbols),
         .url = std::string("fuchsia-pkg://fuchsia.com/driver#meta/driver.cm"),
         .program = std::move(program),
         .incoming = std::move(ns_entries),
         .outgoing_dir = std::move(outgoing_dir_endpoints->server),
         .config = std::nullopt});

    // Start driver.
    std::unique_ptr<compat::Driver> compat_driver;
    auto result = compat::DriverFactory::CreateDriver(
        std::move(start_args), driver_dispatcher_.driver_dispatcher().borrow());
    EXPECT_EQ(ZX_OK, result.status_value());
    auto* driver = result.value().release();
    compat_driver.reset(static_cast<compat::Driver*>(driver));
    return compat_driver;
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

  void AssertDevfsPaths(std::unordered_set<std::string> expected) const {
    exporter_.AssertDevfsPaths(expected);
  }

  void WaitForChildDeviceAdded() {
    while (node().children().empty()) {
      fdf_testing_run_until_idle();
    }
    EXPECT_FALSE(node().children().empty());
  }

  TestProfileProvider profile_provider_;

  async_dispatcher_t* dispatcher() { return driver_dispatcher_.dispatcher(); }

 private:
  TestRootResource root_resource_;
  mock_boot_arguments::Server boot_args_;
  TestItems items_;
  TestDevice test_device_;
  TestFile compat_file_;
  TestFile v1_test_file_;
  TestFile firmware_file_;
  TestDirectory pkg_directory_;
  TestExporter exporter_;
  TestSystemStateTransition system_state_transition_;
  std::optional<fs::ManagedVfs> vfs_;
  fdf::TestSynchronizedDispatcher driver_dispatcher_;
  std::optional<fdf_testing::TestNode> node_;

  // This loop is for FIDL servers that get called in a sync fashion from
  // the driver.
  async::Loop fidl_loop_ = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
};

TEST_F(DriverTest, Start) {
  zx_protocol_device_t ops{
      .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
  };
  auto driver = StartDriver("/pkg/driver/v1_test.so", &ops);

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
  AssertDevfsPaths({"/dev/test/my-device/v1"});

  // Verify v1_test.so state after release.
  UnbindAndFreeDriver(std::move(driver));
  EXPECT_TRUE(v1_test->did_release);
}

TEST_F(DriverTest, Start_WithCreate) {
  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_create_test.so", &ops);

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
  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_missing_test.so", &ops);

  // We will know the driver has finished starting when it closes its node in error.
  while (node().HasNode()) {
    fdf_testing_run_until_idle();
  }
  EXPECT_TRUE(node().children().empty());

  // Verify that v1_test.so has not set a context.
  EXPECT_EQ(nullptr, driver->Context());

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, Start_DeviceAddNull) {
  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_device_add_null_test.so", &ops);

  // Verify that v1_test.so has added a child device.
  WaitForChildDeviceAdded();

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, Start_CheckCompatService) {
  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_device_add_null_test.so", &ops);

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

  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_device_add_null_test.so", &ops);
  zx_handle_t resource2 = get_root_resource();

  // Check that the root resource's value did not change.
  ASSERT_EQ(resource, resource2);
}

TEST_F(DriverTest, Start_GetBackingMemory) {
  compat_file().SetStatus(ZX_ERR_UNAVAILABLE);

  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_test.so", &ops);

  // Verify that v1_test.so has not added a child device.
  EXPECT_TRUE(node().children().empty());

  // Verify that v1_test.so has not set a context.
  EXPECT_EQ(nullptr, driver->Context());

  UnbindAndFreeDriver(std::move(driver));
}

TEST_F(DriverTest, Start_BindFailed) {
  zx_protocol_device_t ops{};
  auto driver = StartDriver("/pkg/driver/v1_test.so", &ops);

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
  zx_protocol_device_t ops{
      .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
  };
  auto driver = StartDriver("/pkg/driver/v1_test.so", &ops);
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
  profile_provider_.SetSetProfileByRoleCallback(
      [](TestProfileProvider::SetProfileByRoleRequestView& rv) {
        ASSERT_TRUE(rv->thread.is_valid());
        ASSERT_EQ("test-profile", rv->role.get());
      });

  zx_protocol_device_t ops{
      .get_protocol = [](void*, uint32_t, void*) { return ZX_OK; },
  };
  auto driver = StartDriver("/pkg/driver/v1_test.so", &ops);
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

  UnbindAndFreeDriver(std::move(driver));
}
