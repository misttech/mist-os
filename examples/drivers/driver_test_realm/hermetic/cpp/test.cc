// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.sample/cpp/wire.h>
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

// [START example]
class DriverTestRealmTest : public gtest::TestLoopFixture {};

TEST_F(DriverTestRealmTest, DriversExist) {
  // Create and build the realm.
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);
  auto realm = realm_builder.Build(dispatcher());

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  ASSERT_EQ(ZX_OK, realm.component().Connect(driver_test_realm.NewRequest()));
  fuchsia::driver::test::Realm_Start_Result realm_result;
  ASSERT_EQ(ZX_OK, driver_test_realm->Start(fuchsia::driver::test::RealmArgs(), &realm_result));
  ASSERT_FALSE(realm_result.is_err());

  // Connect to dev.
  fidl::InterfaceHandle<fuchsia::io::Node> dev;
  zx_status_t status = realm.component().exposed()->Open(fuchsia::io::OpenFlags::RIGHT_READABLE, 0,
                                                         "dev", dev.NewRequest());
  ASSERT_EQ(status, ZX_OK);

  fbl::unique_fd root_fd;
  status = fdio_fd_create(dev.TakeChannel().release(), root_fd.reset_and_get_address());
  ASSERT_EQ(status, ZX_OK);

  // Wait for driver.
  zx::result channel =
      device_watcher::RecursiveWaitForFile(root_fd.get(), "sys/test/sample_driver");
  ASSERT_EQ(channel.status_value(), ZX_OK);

  // Turn the connection into FIDL.
  fidl::WireSyncClient client(
      fidl::ClientEnd<fuchsia_hardware_sample::Echo>(std::move(channel.value())));

  // Send a FIDL request.
  std::string_view sent_string = "hello";
  auto result = client->EchoString(fidl::StringView::FromExternal(sent_string));
  ASSERT_EQ(ZX_OK, result.status());
  ASSERT_EQ(sent_string, result.value().response.get());
}
// [END example]
