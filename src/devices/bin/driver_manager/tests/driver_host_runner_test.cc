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
#include "src/devices/bin/driver_manager/tests/test_pkg.h"

namespace {

namespace fcomponent = fuchsia_component;
namespace fdata = fuchsia_data;
namespace fdfw = fuchsia_driver_framework;
namespace fdecl = fuchsia_component_decl;
namespace fio = fuchsia::io;
namespace frunner = fuchsia_component_runner;

// Returns the exit status of the process.
// TODO(https://fxbug.dev/349913885): this will eventually be included in the bootstrap halper
// library.
int64_t WaitForProcessExit(const zx::process& process);

class DriverHostRunnerTest : public gtest::TestLoopFixture {
  void SetUp() {
    dynamic_linker_ = driver_loader::Loader::Create(dispatcher());
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

  fidl::ClientEnd<fuchsia_driver_loader::DriverHostLauncher> ConnectToDynamicLinker();

  driver_runner::TestRealm& realm() { return realm_; }

 private:
  driver_runner::TestRealm realm_;
  std::optional<fidl::ServerBinding<fuchsia_component::Realm>> realm_binding_;

  std::unique_ptr<driver_loader::Loader> dynamic_linker_;
  std::unique_ptr<driver_manager::DriverHostRunner> driver_host_runner_;

  driver_runner::TestDirectory driver_host_dir_{dispatcher()};
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

  auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

  realm().SetOpenExposedDirHandler(
      [this, kCollection, kDriverHostName](fdecl::ChildRef child, auto exposed_dir) {
        EXPECT_EQ(kCollection, child.collection().value_or(""));
        EXPECT_EQ(kDriverHostName, child.name().substr(0, kDriverHostName.size()));
        driver_host_dir_.Bind(std::move(exposed_dir));
      });

  fidl::WireSharedClient<fuchsia_driver_loader::DriverHostLauncher> launcher(
      ConnectToDynamicLinker(), dispatcher());

  std::shared_ptr<bool> connected = std::make_shared<bool>(false);
  bool got_cb = false;
  driver_host_runner_->StartDriverHost(
      std::move(launcher), std::move(endpoints.server), connected,
      [&](zx::result<fidl::ClientEnd<fuchsia_driver_loader::DriverHost>> result) {
        ASSERT_EQ(ZX_OK, result.status_value());
        ASSERT_TRUE(result->is_valid());
        got_cb = true;
      });

  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(created_component);

  auto pkg_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  test_utils::TestPkg test_pkg(std::move(pkg_endpoints.server), driver_host_path,
                               "bin/driver_host2", expected_libs);
  ASSERT_NO_FATAL_FAILURE(driver_runner::DriverHostComponentStart(realm(), *driver_host_runner_,
                                                                  std::move(pkg_endpoints.client)));
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(got_cb);
  ASSERT_TRUE(*connected);

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

fidl::ClientEnd<fuchsia_driver_loader::DriverHostLauncher>
DriverHostRunnerTest::ConnectToDynamicLinker() {
  auto [client_end, server_end] =
      fidl::Endpoints<fuchsia_driver_loader::DriverHostLauncher>::Create();
  dynamic_linker_->Connect(std::move(server_end));
  return std::move(client_end);
}

int64_t WaitForProcessExit(const zx::process& process) {
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

class DynamicLinkingTest : public driver_runner::DriverRunnerTest {
 public:
  void SetUp() {
    auto driver_host_runner =
        std::make_unique<driver_manager::DriverHostRunner>(dispatcher(), ConnectToRealm());

    SetupDriverRunnerWithDynamicLinker(dispatcher(), std::move(driver_host_runner),
                                       1u /* wait_for_num_drivers */);
  }
};

TEST_F(DynamicLinkingTest, StartRootDriver) {
  auto root_driver = StartRootDriverDynamicLinking();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::unordered_set<const driver_manager::DriverHostRunner::DriverHost*> driver_hosts =
      driver_runner().driver_host_runner_for_tests()->DriverHosts();
  ASSERT_EQ(1u, driver_hosts.size());

  const zx::process& process = (*driver_hosts.begin())->process();
  ASSERT_EQ(24, WaitForProcessExit(process));

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({driver_runner::CreateChildRef("dev", "boot-drivers")});
}

}  // namespace
