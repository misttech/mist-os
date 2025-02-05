// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gizmo.protocol/cpp/fidl.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/directory.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>

#include <thread>

#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

TEST(DriverTransportTest, ParentChildExists) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  // Create and build the realm.
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);
  auto realm = realm_builder.Build(loop.dispatcher());

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  ASSERT_EQ(ZX_OK, realm.component().Connect(driver_test_realm.NewRequest()));

  auto args = fuchsia::driver::test::RealmArgs();
  args.set_root_driver("fuchsia-boot:///dtr#meta/test-parent-sys.cm");

  fuchsia::driver::test::Realm_Start_Result realm_result;
  ASSERT_EQ(ZX_OK, driver_test_realm->Start(std::move(args), &realm_result));
  ASSERT_FALSE(realm_result.is_err());

  fbl::unique_fd fd;
  auto exposed = realm.component().CloneExposedDir();
  ASSERT_EQ(fdio_fd_create(exposed.TakeChannel().release(), fd.reset_and_get_address()), ZX_OK);

  {
    // Wait for parent driver.
    zx::result channel =
        device_watcher::RecursiveWaitForFile(fd.get(), "dev-topological/sys/test/transport-parent");
    ASSERT_EQ(channel.status_value(), ZX_OK);
  }

  {
    // Wait for child driver.
    zx::result channel = device_watcher::RecursiveWaitForFile(
        fd.get(), "dev-topological/sys/test/transport-parent/transport-child");
    ASSERT_EQ(channel.status_value(), ZX_OK);
  }
}
