// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/driver/test/cpp/fidl.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>

#include <fbl/unique_fd.h>

#include "lib/fdio/fd.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace {

class FidlServiceTest : public gtest::TestLoopFixture {};

TEST_F(FidlServiceTest, ChildBinds) {
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);
  auto realm = realm_builder.Build(dispatcher());

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  ASSERT_EQ(ZX_OK, realm.component().Connect(driver_test_realm.NewRequest()));
  fuchsia::driver::test::Realm_Start_Result realm_result;

  auto args = fuchsia::driver::test::RealmArgs();
  args.set_root_driver("fuchsia-boot:///dtr#meta/test-parent-sys.cm");
  ASSERT_EQ(ZX_OK, driver_test_realm->Start(std::move(args), &realm_result));
  ASSERT_FALSE(realm_result.is_err());

  fbl::unique_fd fd;
  auto exposed = realm.component().CloneExposedDir();
  ASSERT_EQ(fdio_fd_create(exposed.TakeChannel().release(), fd.reset_and_get_address()), ZX_OK);

  // Wait for the child device to bind and appear. The child driver should bind with its string
  // properties. It will then make a call via FIDL and wait for the response before adding the child
  // device.
  zx::result channel =
      device_watcher::RecursiveWaitForFile(fd.get(), "dev-topological/sys/test/parent/child");
  ASSERT_EQ(channel.status_value(), ZX_OK);
}

}  // namespace
