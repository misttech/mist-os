// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/driver/test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/fd.h>
#include <lib/fidl/cpp/synchronous_interface_ptr.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>

#include <fbl/unique_fd.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

class CompositeTest : public gtest::TestLoopFixture, public testing::WithParamInterface<bool> {};

TEST_P(CompositeTest, DriversExist) {
  // Create and build the realm.
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);
  auto realm = realm_builder.Build(dispatcher());

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  ASSERT_EQ(ZX_OK, realm.Connect(driver_test_realm.NewRequest()));
  fuchsia::driver::test::Realm_Start_Result realm_result;

  fuchsia::driver::test::RealmArgs args;
  if (GetParam()) {
    args.set_use_driver_framework_v2(true);
    args.set_root_driver("fuchsia-boot:///#meta/test-parent-sys.cm");
  }

  ASSERT_EQ(ZX_OK, driver_test_realm->Start(std::move(args), &realm_result));
  ASSERT_FALSE(realm_result.is_err());

  // Connect to dev.
  fidl::InterfaceHandle<fuchsia::io::Node> dev;
  zx_status_t status = realm.component().exposed()->Open(fuchsia::io::OpenFlags::RIGHT_READABLE, 0,
                                                         "dev", dev.NewRequest());
  ASSERT_EQ(status, ZX_OK);

  fbl::unique_fd root_fd;
  status = fdio_fd_create(dev.TakeChannel().release(), root_fd.reset_and_get_address());
  ASSERT_EQ(status, ZX_OK);

  EXPECT_EQ(device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/child_a").status_value(),
            ZX_OK);
  EXPECT_EQ(device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/child_b").status_value(),
            ZX_OK);
  EXPECT_EQ(device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/child_c").status_value(),
            ZX_OK);
  EXPECT_EQ(device_watcher::RecursiveWaitForFile(
                root_fd.get(), "sys/test/child_a/composite_driver_v1/composite_child")
                .status_value(),
            ZX_OK);

  EXPECT_EQ(
      device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/fragment_a").status_value(),
      ZX_OK);
  EXPECT_EQ(
      device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/fragment_b").status_value(),
      ZX_OK);
  EXPECT_EQ(
      device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/fragment_c").status_value(),
      ZX_OK);
  EXPECT_EQ(device_watcher::RecursiveWaitForFile(
                root_fd.get(), "sys/test/fragment_a/composite-device/composite_child")
                .status_value(),
            ZX_OK);
}

INSTANTIATE_TEST_SUITE_P(CompositeTest, CompositeTest, testing::Bool(),
                         [](const testing::TestParamInfo<bool>& info) {
                           if (info.param) {
                             return "DFv2";
                           }
                           return "DFv1";
                         });
