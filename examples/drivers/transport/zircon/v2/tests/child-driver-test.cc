// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/zircon/v2/child-driver.h"

#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

namespace testing {

namespace {
constexpr uint32_t kTestHardwareId = 0x1234567;
constexpr uint32_t kTestMajorVersion = 0x9;
constexpr uint32_t kTestMinorVersion = 0x5;
}  // namespace

class FakeGizmoServer : public fidl::WireServer<fuchsia_examples_gizmo::Device> {
 public:
  void GetHardwareId(GetHardwareIdCompleter::Sync& completer) override {
    completer.ReplySuccess(kTestHardwareId);
  }

  void GetFirmwareVersion(GetFirmwareVersionCompleter::Sync& completer) override {
    completer.ReplySuccess(kTestMajorVersion, kTestMinorVersion);
  }
};

class ZirconTransportTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    fuchsia_examples_gizmo::Service::InstanceHandler handler({
        .device =
            bindings_.CreateHandler(&server_, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                    fidl::kIgnoreBindingClosure),
    });
    auto result = to_driver_vfs.AddService<fuchsia_examples_gizmo::Service>(std::move(handler));
    EXPECT_EQ(ZX_OK, result.status_value());
    return zx::ok();
  }

 private:
  FakeGizmoServer server_;
  fidl::ServerBindingGroup<fuchsia_examples_gizmo::Device> bindings_;
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = zircon_transport::ChildZirconTransportDriver;
  using EnvironmentType = ZirconTransportTestEnvironment;
};

class ChildZirconTransportDriverTest : public fdf_testing::DriverTestFixture<FixtureConfig> {};

TEST_F(ChildZirconTransportDriverTest, VerifyQueryValues) {
  // Verify that the queried values match the fake parent driver server.
  EXPECT_EQ(kTestHardwareId, driver()->hardware_id());
  EXPECT_EQ(kTestMajorVersion, driver()->major_version());
  EXPECT_EQ(kTestMinorVersion, driver()->minor_version());
}

}  // namespace testing
