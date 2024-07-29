// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/driver_host_runner.h"

#include <fidl/fuchsia.component.decl/cpp/test_base.h>
#include <fidl/fuchsia.component/cpp/test_base.h>
#include <fuchsia/io/cpp/fidl_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/binding.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/devices/bin/driver_loader/loader.h"
#include "src/devices/bin/driver_manager/tests/driver_runner_test_fixture.h"

namespace {

namespace fcomponent = fuchsia_component;
namespace fdata = fuchsia_data;
namespace fdfw = fuchsia_driver_framework;
namespace fdecl = fuchsia_component_decl;
namespace fio = fuchsia::io;
namespace frunner = fuchsia_component_runner;

void CallComponentStart(driver_runner::TestRealm& realm,
                        driver_manager::DriverHostRunner& driver_host_runner,
                        std::string_view driver_host_path,
                        const std::vector<std::string_view> expected_libs);

class TestTransaction : public fidl::Transaction {
 private:
  std::unique_ptr<Transaction> TakeOwnership() override {
    return std::make_unique<TestTransaction>();
  }

  zx_status_t Reply(fidl::OutgoingMessage* message, fidl::WriteOptions write_options) override {
    EXPECT_TRUE(false);
    return ZX_OK;
  }

  void Close(zx_status_t epitaph) override { EXPECT_TRUE(false); }
};

class TestFile : public fio::testing::File_TestBase {
 public:
  explicit TestFile(std::string_view path) : path_(std::move(path)) {}

 private:
  void GetBackingMemory(fio::VmoFlags flags, GetBackingMemoryCallback callback) override {
    EXPECT_EQ(fio::VmoFlags::READ | fio::VmoFlags::EXECUTE | fio::VmoFlags::PRIVATE_CLONE, flags);
    auto endpoints = fidl::Endpoints<fuchsia_io::File>::Create();
    EXPECT_EQ(ZX_OK, fdio_open(path_.data(),
                               static_cast<uint32_t>(fio::OpenFlags::RIGHT_READABLE |
                                                     fio::OpenFlags::RIGHT_EXECUTABLE),
                               endpoints.server.channel().release()));

    fidl::WireSyncClient<fuchsia_io::File> file(std::move(endpoints.client));
    fidl::WireResult result = file->GetBackingMemory(fuchsia_io::wire::VmoFlags(uint32_t(flags)));
    EXPECT_TRUE(result.ok()) << result.FormatDescription();
    auto* res = result.Unwrap();
    if (res->is_error()) {
      callback(fio::File_GetBackingMemory_Result::WithErr(std::move(res->error_value())));
      return;
    }
    callback(fio::File_GetBackingMemory_Result::WithResponse(
        fio::File_GetBackingMemory_Response(std::move(res->value()->vmo))));
  }

  void NotImplemented_(const std::string& name) override {
    printf("Not implemented: File::%s\n", name.data());
  }

  std::string path_;
};

class TestDirectory : public fio::testing::Directory_TestBase {
 public:
  using OpenHandler = fit::function<void(fio::OpenFlags flags, std::string path,
                                         fidl::InterfaceRequest<fio::Node> object)>;

  void SetOpenHandler(OpenHandler open_handler) { open_handler_ = std::move(open_handler); }

 private:
  void Open(fio::OpenFlags flags, fio::ModeType mode, std::string path,
            fidl::InterfaceRequest<fio::Node> object) override {
    open_handler_(flags, std::move(path), std::move(object));
  }

  void NotImplemented_(const std::string& name) override {
    printf("Not implemented: Directory::%s\n", name.data());
  }

  OpenHandler open_handler_;
};

// Implementation of a /pkg directory that can be passed as a component namespace entry
// for the started driver host or driver component.
class TestPkg {
 public:
  // |server| is the channel that will be served by |TestPkg|.
  //
  // |module_test_pkg_path| is where the module is located in the test's package. e.g.
  // /pkg/bin/driver_host2.
  //
  // |module_open_path| is the path that will be requested to the /pkg open
  // handler for the module. e.g. bin/driver_host2.
  //
  // |expected_libs| holds that names of the libraries that are needed by the module.
  // This list will be used to construct the test files that the driver host runner
  // or driver runner expects to be present in the "/pkg/libs" dir that will be passed
  // to the dynamic linker. No additional validation is done on the strings in |expected_libs|.
  TestPkg(fidl::ServerEnd<fuchsia_io::Directory> server, std::string_view module_test_pkg_path,
          std::string_view module_open_path, const std::vector<std::string_view> expected_libs);

  ~TestPkg() {
    loop_.Quit();
    loop_.JoinThreads();
  }

 private:
  static constexpr std::string_view kLibPathPrefix = "/pkg/lib/";

  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};

  TestDirectory pkg_dir_;
  fidl::Binding<fio::Directory> pkg_binding_{&pkg_dir_};

  TestDirectory lib_dir_;
  fidl::Binding<fio::Directory> lib_dir_binding_{&lib_dir_};
  std::map<std::string, TestFile> libname_to_file_;
  std::vector<std::unique_ptr<fidl::Binding<fio::File>>> lib_file_bindings_;

  TestFile module_;
  fidl::Binding<fio::File> module_binding_{&module_};
};

TestPkg::TestPkg(fidl::ServerEnd<fuchsia_io::Directory> server,
                 std::string_view module_test_pkg_path, std::string_view module_open_path,
                 const std::vector<std::string_view> expected_libs)
    : module_(module_test_pkg_path) {
  EXPECT_EQ(ZX_OK, loop_.StartThread());

  // Construct the test files for the expected libs.
  for (auto name : expected_libs) {
    auto path = std::string(kLibPathPrefix).append(name);
    libname_to_file_.emplace(std::piecewise_construct, std::forward_as_tuple(name),
                             std::forward_as_tuple(path.c_str()));
  }

  lib_dir_.SetOpenHandler([this](fio::OpenFlags flags, std::string path, auto object) {
    EXPECT_EQ(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE, flags);
    auto it = libname_to_file_.find(path);
    EXPECT_NE(it, libname_to_file_.end());
    lib_file_bindings_.push_back(std::make_unique<fidl::Binding<fio::File>>(
        &(it->second), object.TakeChannel(), loop_.dispatcher()));
  });

  pkg_binding_.Bind(server.TakeChannel(), loop_.dispatcher());
  pkg_dir_.SetOpenHandler([this, module_open_path = std::string(module_open_path)](
                              fio::OpenFlags flags, std::string path, auto object) {
    if (strcmp(path.c_str(), "lib") == 0) {
      EXPECT_EQ(fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE |
                    fio::OpenFlags::RIGHT_EXECUTABLE,
                flags);
      lib_dir_binding_.Bind(object.TakeChannel(), loop_.dispatcher());

    } else if (strcmp(path.c_str(), module_open_path.c_str()) == 0) {
      EXPECT_EQ(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE, flags);
      module_binding_.Bind(object.TakeChannel(), loop_.dispatcher());
    }
  });
}

class DriverHostRunnerTest : public gtest::TestLoopFixture {
  void SetUp() {
    loader_ = driver_loader::Loader::Create();
    driver_host_runner_ =
        std::make_unique<driver_manager::DriverHostRunner>(dispatcher(), ConnectToRealm());
  }

 protected:
  // Creates the driver host component, loads the driver host and waits for it to exit.
  // |driver_host_path| is the local package path to the binary to pass to the driver host runner.
  //
  // |expected_libs| holds that names of the libraries that are needed by the driver host.
  // This list will be used to construct the test files that the driver host runner expects
  // to be present in the "/pkg/libs" dir that will be passed to the dynamic linker.
  // No additional validation is done on the strings in |expected_libs|.
  void StartDriverHost(std::string_view driver_host_path,
                       const std::vector<std::string_view> expected_libs);

  fidl::ClientEnd<fuchsia_component::Realm> ConnectToRealm();

  // Returns the exit status of the process.
  int64_t WaitForProcessExit(const zx::process& process);

  driver_runner::TestRealm& realm() { return realm_; }

 private:
  driver_runner::TestRealm realm_;
  std::optional<fidl::ServerBinding<fuchsia_component::Realm>> realm_binding_;

  std::unique_ptr<driver_loader::Loader> loader_;
  std::unique_ptr<driver_manager::DriverHostRunner> driver_host_runner_;
};

void DriverHostRunnerTest::StartDriverHost(std::string_view driver_host_path,
                                           const std::vector<std::string_view> expected_libs) {
  constexpr std::string_view kDriverHostName = "driver-host-new-";
  constexpr std::string_view kCollection = "driver-hosts";
  constexpr std::string_view kComponentUrl = "fuchsia-boot:///driver_host2#meta/driver_host2.cm";

  bool created_component;
  realm().SetCreateChildHandler(
      [&](fdecl::CollectionRef collection, fdecl::Child decl, std::vector<fdecl::Offer> offers) {
        EXPECT_EQ(kDriverHostName, decl.name().value().substr(0, kDriverHostName.size()));
        EXPECT_EQ(kCollection, collection.name());
        EXPECT_EQ(kComponentUrl, decl.url());
        created_component = true;
      });

  zx::channel bootstrap_sender, bootstrap_receiver;
  zx_status_t status = zx::channel::create(0, &bootstrap_sender, &bootstrap_receiver);
  ASSERT_EQ(ZX_OK, status);

  // TODO(https://fxbug.dev/340928556): we should pass a channel to the loader rather than the
  // entire thing.
  bool got_cb = false;
  driver_host_runner_->StartDriverHost(loader_.get(), std::move(bootstrap_receiver),
                                       [&](zx::result<> result) {
                                         ASSERT_EQ(ZX_OK, result.status_value());
                                         got_cb = true;
                                       });

  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(created_component);

  ASSERT_NO_FATAL_FAILURE(
      CallComponentStart(realm(), *driver_host_runner_, driver_host_path, expected_libs));
  ASSERT_TRUE(got_cb);

  std::unordered_set<const driver_manager::DriverHostRunner::DriverHost*> driver_hosts =
      driver_host_runner_->DriverHosts();
  ASSERT_EQ(1u, driver_hosts.size());

  const zx::process& process = (*driver_hosts.begin())->process();
  ASSERT_EQ(0, WaitForProcessExit(process));
}

fidl::ClientEnd<fuchsia_component::Realm> DriverHostRunnerTest::ConnectToRealm() {
  auto realm_endpoints = fidl::Endpoints<fcomponent::Realm>::Create();
  realm_binding_.emplace(dispatcher(), std::move(realm_endpoints.server), &realm_,
                         fidl::kIgnoreBindingClosure);
  return std::move(realm_endpoints.client);
}

void CallComponentStart(driver_runner::TestRealm& realm,
                        driver_manager::DriverHostRunner& driver_host_runner,
                        std::string_view driver_host_path,
                        const std::vector<std::string_view> expected_libs) {
  fidl::Arena arena;

  fidl::VectorView<fdata::wire::DictionaryEntry> program_entries(arena, 1);
  program_entries[0].key.Set(arena, "binary");
  program_entries[0].value = fdata::wire::DictionaryValue::WithStr(arena, "bin/driver_host2");
  auto program_builder = fdata::wire::Dictionary::Builder(arena);
  program_builder.entries(program_entries);

  auto pkg_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  TestPkg test_pkg(std::move(pkg_endpoints.server), driver_host_path, "bin/driver_host2",
                   expected_libs);

  fidl::VectorView<frunner::wire::ComponentNamespaceEntry> ns_entries(arena, 1);
  ns_entries[0] = frunner::wire::ComponentNamespaceEntry::Builder(arena)
                      .path("/pkg")
                      .directory(std::move(pkg_endpoints.client))
                      .Build();

  auto start_info_builder = frunner::wire::ComponentStartInfo::Builder(arena);
  start_info_builder.resolved_url("fuchsia-boot:///driver_host2#meta/driver_host2.cm")
      .program(program_builder.Build())
      .ns(ns_entries)
      .numbered_handles(realm.TakeHandles(arena));

  auto controller_endpoints = fidl::Endpoints<frunner::ComponentController>::Create();
  TestTransaction transaction;
  {
    fidl::WireServer<frunner::ComponentRunner>::StartCompleter::Sync completer(&transaction);
    fidl::WireRequest<frunner::ComponentRunner::Start> request{
        start_info_builder.Build(), std::move(controller_endpoints.server)};
    static_cast<fidl::WireServer<frunner::ComponentRunner>&>(driver_host_runner)
        .Start(&request, completer);
  }
}

int64_t DriverHostRunnerTest::WaitForProcessExit(const zx::process& process) {
  int64_t result = -1;

  auto wait_for_termination = [&process, &result]() {
    zx_signals_t signals;
    ASSERT_EQ(process.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), &signals), ZX_OK);
    ASSERT_TRUE(signals & ZX_PROCESS_TERMINATED);
    zx_info_process_t info;
    ASSERT_EQ(process.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr), ZX_OK);
    ASSERT_TRUE(info.flags & ZX_INFO_PROCESS_FLAG_STARTED);
    ASSERT_TRUE(info.flags & ZX_INFO_PROCESS_FLAG_EXITED);
    result = info.return_code;
  };
  wait_for_termination();

  return result;
}

TEST_F(DriverHostRunnerTest, StartDriverHost) {
  constexpr std::string_view kDriverHostPath = "/pkg/bin/driver_host2";
  const std::vector<std::string_view> kExpectedLibs;
  StartDriverHost(kDriverHostPath, kExpectedLibs);
}

TEST_F(DriverHostRunnerTest, StartFakeDriverHost) {
  constexpr std::string_view kDriverHostPath = "/pkg/bin/fake_driver_host";
  const std::vector<std::string_view> kExpectedLibs = {
      "libdh-deps-a.so",
      "libdh-deps-b.so",
      "libdh-deps-c.so",
  };
  StartDriverHost(kDriverHostPath, kExpectedLibs);
}

class DynamicLinkingTest : public driver_runner::DriverRunnerTest {};

TEST_F(DynamicLinkingTest, StartRootDriver) {
  constexpr std::string_view kDriverHostPath = "/pkg/bin/driver_host2";
  // TODO(https://fxbug.dev/341998660): setup fake driver and expected /pkg.
  const std::string kRootDriverBinary = "driver/fake_driver.so";

  PrepareRealmForDriverComponentStart("dev", driver_runner::root_driver_url);

  auto driver_host_runner =
      std::make_unique<driver_manager::DriverHostRunner>(dispatcher(), ConnectToRealm());

  SetupDriverRunnerWithDynamicLinker(std::move(driver_host_runner));

  auto start = driver_runner().StartRootDriver(driver_runner::root_driver_url);
  ASSERT_FALSE(start.is_error());
  EXPECT_TRUE(RunLoopUntilIdle());

  StartDriverHandler start_handler = [kRootDriverBinary](driver_runner::TestDriver* driver,
                                                         fdfw::DriverStartArgs start_args) {
    ValidateProgram(start_args.program(), kRootDriverBinary, "false" /* colocate */,
                    "false" /* host_restart_on_crash */, "false" /* use_next_vdso */,
                    "true" /* use_dynamic_linker */);
  };
  // TODO(https://fxbug.dev/341998660): currently starting the driver has not been
  // completely implemented, so the returned StartDriverResult is not very useful.
  [[maybe_unused]] auto root_driver = StartDriver(
      {
          .url = driver_runner::root_driver_url,
          .binary = kRootDriverBinary,
          .use_dynamic_linker = true,
      },
      std::move(start_handler));

  const std::vector<std::string_view> kExpectedLibs;
  ASSERT_NO_FATAL_FAILURE(CallComponentStart(
      realm(), *driver_runner().driver_host_runner_for_tests(), kDriverHostPath, kExpectedLibs));

  std::unordered_set<const driver_manager::DriverHostRunner::DriverHost*> driver_hosts =
      driver_runner().driver_host_runner_for_tests()->DriverHosts();
  ASSERT_EQ(1u, driver_hosts.size());
}

}  // namespace
