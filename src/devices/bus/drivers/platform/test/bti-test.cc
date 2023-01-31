// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.hardware.btitest/cpp/wire.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/zx/time.h>
#include <zircon/boot/image.h>
#include <zircon/status.h>

#include <zxtest/zxtest.h>

namespace {

using device_watcher::RecursiveWaitForFile;

using namespace component_testing;

constexpr char kParentPath[] = "sys/platform/11:01:1a";
constexpr char kDevicePath[] = "sys/platform/11:01:1a/test-bti";

TEST(PbusBtiTest, BtiIsSameAfterCrash) {
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);
  realm_builder.AddRoute(Route{.capabilities = {Protocol{"fuchsia.boot.RootResource"}},
                               .source = {ParentRef()},
                               .targets = {ChildRef{"driver_test_realm"}}});

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto realm = realm_builder.Build(loop.dispatcher());

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  ASSERT_EQ(ZX_OK, realm.component().Connect(driver_test_realm.NewRequest()));
  fuchsia::driver::test::Realm_Start_Result realm_result;
  auto args = fuchsia::driver::test::RealmArgs();
  args.set_root_driver("fuchsia-boot:///#driver/platform-bus.so");
  ASSERT_EQ(ZX_OK, driver_test_realm->Start(std::move(args), &realm_result));
  ASSERT_FALSE(realm_result.is_err());

  // Connect to dev.
  fidl::InterfaceHandle<fuchsia::io::Node> dev;
  zx_status_t status = realm.component().exposed()->Open(fuchsia::io::OpenFlags::RIGHT_READABLE, 0,
                                                         "dev", dev.NewRequest());
  ASSERT_EQ(status, ZX_OK);

  fbl::unique_fd dev_fd;
  status = fdio_fd_create(dev.TakeChannel().release(), dev_fd.reset_and_get_address());
  ASSERT_EQ(status, ZX_OK);

  zx::result channel = RecursiveWaitForFile(dev_fd.get(), kDevicePath);
  EXPECT_OK(channel.status_value());
  fidl::ClientEnd<fuchsia_hardware_btitest::BtiDevice> bti_client_end(std::move(channel.value()));

  fidl::WireSyncClient client(std::move(bti_client_end));
  uint64_t koid1;
  {
    auto result = client->GetKoid();
    ASSERT_OK(result.status());
    koid1 = result.value().koid;
  }

  fbl::unique_fd watch_fd(openat(dev_fd.get(), kParentPath, O_DIRECTORY | O_RDONLY));
  std::unique_ptr<device_watcher::DirWatcher> watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(watch_fd.get(), &watcher));

  {
    auto result = client->Crash();
    ASSERT_OK(result.status());
  }

  // We implicitly rely on driver host being rebound in the event of a crash.
  ASSERT_OK(watcher->WaitForRemoval("test-bti", zx::duration::infinite()));
  channel = RecursiveWaitForFile(dev_fd.get(), kDevicePath);
  ASSERT_OK(channel.status_value());
  bti_client_end = fidl::ClientEnd<fuchsia_hardware_btitest::BtiDevice>(std::move(channel.value()));
  client = fidl::WireSyncClient(std::move(bti_client_end));

  uint64_t koid2;
  {
    auto result = client->GetKoid();
    ASSERT_OK(result.status());
    koid2 = result.value().koid;
  }
  ASSERT_EQ(koid1, koid2);
}

}  // namespace
