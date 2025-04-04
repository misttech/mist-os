// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sample/cpp/wire.h>
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
  zx::result dtr_client = realm.component().Connect<fuchsia_driver_test::Realm>();
  ASSERT_EQ(ZX_OK, dtr_client.status_value());
  fidl::Result realm_result = fidl::Call(*dtr_client)->Start({});
  ASSERT_TRUE(realm_result.is_ok()) << realm_result.error_value().FormatDescription();

  fbl::unique_fd fd;
  auto exposed = realm.component().CloneExposedDir();
  ASSERT_EQ(ZX_OK, fdio_fd_create(exposed.TakeChannel().release(), fd.reset_and_get_address()));

  // Wait for driver.
  zx::result channel =
      device_watcher::RecursiveWaitForFile(fd.get(), "dev-topological/sys/test/sample_driver");
  ASSERT_EQ(channel.status_value(), ZX_OK);

  fidl::ClientEnd<fuchsia_hardware_sample::Echo> client(std::move(*channel));

  // Send a FIDL request.
  constexpr std::string_view sent_string = "hello";
  fidl::WireResult result =
      fidl::WireCall(client)->EchoString(fidl::StringView::FromExternal(sent_string));
  ASSERT_EQ(ZX_OK, result.status());
  ASSERT_EQ(sent_string, result.value().response.get());
}
// [END example]
