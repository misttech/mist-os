// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/async/cpp/executor.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/driver/power/cpp/testing/fake_activity_governor.h>
#include <lib/driver/power/cpp/testing/fidl_bound_server.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fpromise/result.h>
#include <lib/zx/time.h>

#include <memory>
#include <optional>
#include <string>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "examples/power/wake_lease/cpp/wake_lease.h"

namespace {

using fdf_power::testing::FakeActivityGovernorListener;
using fdf_power::testing::FidlBoundServer;
using fuchsia_power_broker::CurrentLevel;
using fuchsia_power_broker::DependencyToken;
using fuchsia_power_broker::DependencyType;
using fuchsia_power_broker::ElementControl;
using fuchsia_power_broker::ElementSchema;
using fuchsia_power_broker::LeaseControl;
using fuchsia_power_broker::Lessor;
using fuchsia_power_broker::LevelControlChannels;
using fuchsia_power_broker::LevelDependency;
using fuchsia_power_broker::RequiredLevel;
using fuchsia_power_broker::Topology;
using fuchsia_power_broker::wire::BinaryPowerLevel;
using fuchsia_power_system::ActivityGovernor;
using fuchsia_power_system::ActivityGovernorListener;
using fuchsia_power_system::ApplicationActivityLevel;

class WakeLeaseIntegrationTest : public gtest::RealLoopFixture {
 protected:
  template <typename Protocol>
  fidl::Client<Protocol> Connect() {
    zx::result<fidl::ClientEnd<Protocol>> result = component::Connect<Protocol>();
    EXPECT_TRUE(result.is_ok()) << result.status_string();
    return fidl::Client(std::move(*result), dispatcher());
  }
};

class ApplicationActivityElement {
 public:
  explicit ApplicationActivityElement(const std::string& name,
                                      const fidl::Client<ActivityGovernor>& activity_governor,
                                      const fidl::Client<Topology>& topology)
      : current_level_endpoints_(fidl::CreateEndpoints<CurrentLevel>().value()),
        element_control_endpoints_(fidl::CreateEndpoints<ElementControl>().value()),
        lessor_endpoints_(fidl::CreateEndpoints<Lessor>().value()),
        required_level_endpoints_(fidl::CreateEndpoints<RequiredLevel>().value()) {
    activity_governor->GetPowerElements().Then([&](auto& result) {
      EXPECT_TRUE(result.is_ok());
      DependencyToken token =
          std::move(result->application_activity().value().assertive_dependency_token().value());
      ElementSchema schema = BuildAssertiveApplicationActivitySchema(name, std::move(token));
      topology->AddElement(std::move(schema)).Then([](auto& result) {
        EXPECT_TRUE(result.is_ok());
      });
    });
  }

  fidl::ClientEnd<Lessor> TakeLessorClientEnd() { return std::move(lessor_endpoints_.client); }

 private:
  ElementSchema BuildAssertiveApplicationActivitySchema(const std::string& name,
                                                        zx::event requires_token) {
    LevelDependency dependency(
        /*dependency_type=*/DependencyType::kAssertive,
        /*dependent_level=*/fidl::ToUnderlying(BinaryPowerLevel::kOn),
        /*requires_token=*/std::move(requires_token),
        /*requires_level_by_preference=*/
        std::vector<uint8_t>({fidl::ToUnderlying(ApplicationActivityLevel::kActive)}));
    LevelControlChannels level_control_channels(std::move(current_level_endpoints_.server),
                                                std::move(required_level_endpoints_.server));
    ElementSchema schema{{
        .element_name = name,
        .initial_current_level = fidl::ToUnderlying(BinaryPowerLevel::kOn),
        .valid_levels = std::vector<uint8_t>({fidl::ToUnderlying(BinaryPowerLevel::kOff),
                                              fidl::ToUnderlying(BinaryPowerLevel::kOn)}),
        .level_control_channels = std::move(level_control_channels),
        .lessor_channel = std::move(lessor_endpoints_.server),
        .element_control = std::move(element_control_endpoints_.server),
    }};
    schema.dependencies().emplace().push_back(std::move(dependency));
    return schema;
  }

  fidl::Endpoints<CurrentLevel> current_level_endpoints_;
  fidl::Endpoints<ElementControl> element_control_endpoints_;
  fidl::Endpoints<Lessor> lessor_endpoints_;
  fidl::Endpoints<RequiredLevel> required_level_endpoints_;
};

// TODO(b/356953708): This test is currently broken on specific environments.
// Re-enable once fixed.
TEST_F(WakeLeaseIntegrationTest, DISABLED_WakeLeaseBlocksSuspend) {
  auto topology = Connect<fuchsia_power_broker::Topology>();
  auto activity_governor = Connect<fuchsia_power_system::ActivityGovernor>();

  // Take an assertive lease on ApplicationActivity to indicate boot completion.
  // System Activity Governor waits for this signal before handling suspend or resume.
  auto activity_element =
      std::make_unique<ApplicationActivityElement>("boot-complete", activity_governor, topology);
  fidl::Client<Lessor> activity_lessor(activity_element->TakeLessorClientEnd(), dispatcher());
  auto activity_lease_control = std::make_unique<fidl::Client<LeaseControl>>();
  bool lease_completed = false;
  activity_lessor->Lease(fidl::ToUnderlying(ApplicationActivityLevel::kActive))
      .Then([&](auto& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        lease_completed = true;
        activity_lease_control->Bind(std::move(result.value().lease_control()), dispatcher());
      });
  RunLoopUntil([&lease_completed]() { return lease_completed; });
  EXPECT_TRUE(lease_completed);

  // Register a Listener on System Activity Governor to check for suspend callbacks.
  auto endpoints = fidl::CreateEndpoints<ActivityGovernorListener>().value();
  FidlBoundServer<FakeActivityGovernorListener> listener(dispatcher(), std::move(endpoints.server));
  bool register_listener_completed = false;
  activity_governor
      ->RegisterListener({{.listener = std::make_optional(std::move(endpoints.client))}})
      .Then([&register_listener_completed](auto& result) { register_listener_completed = true; });
  RunLoopUntil([&register_listener_completed]() { return register_listener_completed; });
  EXPECT_TRUE(register_listener_completed);
  ASSERT_FALSE(listener.SuspendStarted());

  // Take a wake lease and check that OnSuspend doesn't get called.
  std::unique_ptr<examples::power::WakeLease> wake_lease;
  bool take_wake_lease_completed = false;
  async::Executor executor(dispatcher());
  executor.schedule_task(
      examples::power::WakeLease::Take(activity_governor, "test-wake-lease")
          .then([&](fpromise::result<examples::power::WakeLease, examples::power::Error>& result) {
            EXPECT_FALSE(result.is_error()) << result.error();
            ASSERT_FALSE(listener.SuspendStarted());

            wake_lease = std::make_unique<examples::power::WakeLease>(result.take_value());
            take_wake_lease_completed = true;
          }));
  RunLoopUntil([&take_wake_lease_completed]() { return take_wake_lease_completed; });
  EXPECT_TRUE(take_wake_lease_completed);
  EXPECT_TRUE(wake_lease);

  // Dropping the ApplicationActivity lease shouldn't suspend the system as long as the wake lease
  // is active.
  activity_element.reset();
  activity_lease_control.reset();
  EXPECT_TRUE(activity_lessor.UnbindMaybeGetEndpoint().is_ok());
  RunLoopUntilIdle();
  ASSERT_FALSE(listener.SuspendStarted());

  // Drop the wake lease and observe OnSuspend callback.
  wake_lease.reset();
  EXPECT_FALSE(wake_lease);
  RunLoopUntil([&listener]() { return listener.SuspendStarted(); });
  ASSERT_TRUE(listener.SuspendStarted());
}

}  // namespace
