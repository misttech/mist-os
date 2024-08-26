// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/power/cpp/unmanaged_element.h"

#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/bridge.h>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "examples/power/cpp/testing/scoped_background_loop.h"

namespace {

using examples::power::UnmanagedElement;
using examples::power::testing::ScopedBackgroundLoop;
using fuchsia_power_broker::BinaryPowerLevel;
using fuchsia_power_broker::CurrentLevel;
using fuchsia_power_broker::ElementSchema;
using fuchsia_power_broker::PowerLevel;
using fuchsia_power_broker::Topology;

const std::vector<PowerLevel> kBinaryPowerLevels = {fidl::ToUnderlying(BinaryPowerLevel::kOff),
                                                    fidl::ToUnderlying(BinaryPowerLevel::kOn)};

class UnmanagedElementTest : public gtest::RealLoopFixture {};

class FakeTopology : public fidl::testing::TestBase<Topology> {
 public:
  FakeTopology(async_dispatcher_t* dispatcher, fidl::ServerEnd<Topology> server_end)
      : binding_(
            fidl::BindServer(dispatcher, std::move(server_end), this,
                             [](FakeTopology*, fidl::UnbindInfo, fidl::ServerEnd<Topology>) {})) {}

  fpromise::promise<ElementSchema, void> PromiseSchema() {
    return schema_bridge_.consumer.promise();
  }

 private:
  void AddElement(ElementSchema& schema, AddElementCompleter::Sync& completer) override {
    schema_bridge_.completer.complete_ok(std::move(schema));
    completer.Reply(fit::ok());
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FAIL() << "Unexpected call: " << name;
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<Topology> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FAIL() << "Encountered unknown method";
  }

  fidl::ServerBindingRef<Topology> binding_;
  fpromise::bridge<ElementSchema, void> schema_bridge_;
};

// Must run on a single sequence or single thread.
class FakeCurrentLevel : public fidl::testing::TestBase<CurrentLevel> {
 public:
  FakeCurrentLevel(async_dispatcher_t* dispatcher, fidl::ServerEnd<CurrentLevel> server_end,
                   PowerLevel initial_level)
      : binding_(fidl::BindServer(
            dispatcher, std::move(server_end), this,
            [](FakeCurrentLevel*, fidl::UnbindInfo, fidl::ServerEnd<CurrentLevel>) {})),
        current_level_(initial_level) {}

  // This function can be made const, but then it doesn't work with DispatcherBound::AsyncCall().
  PowerLevel current_level() { return current_level_; }

 private:
  void Update(UpdateRequest& request, UpdateCompleter::Sync& completer) override {
    current_level_ = request.current_level();
    completer.Reply(fit::ok());
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FAIL() << "Unexpected call: " << name;
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<CurrentLevel> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FAIL() << "Encountered unknown method";
  }

  fidl::ServerBindingRef<CurrentLevel> binding_;
  PowerLevel current_level_;
};

TEST_F(UnmanagedElementTest, AddToPowerTopologySendsCorrectSchema) {
  ScopedBackgroundLoop background;

  // Create and run the Topology server in the background.
  auto [topology_client, topology_server] = fidl::Endpoints<Topology>::Create();
  FakeTopology topology(background.dispatcher(), std::move(topology_server));

  // Create an unmanaged element and add it to the power topology.
  const std::string element_name = "unmanaged-element";
  auto _element = UnmanagedElement::Add(topology_client, element_name, kBinaryPowerLevels,
                                        kBinaryPowerLevels[0]);

  // Check the ElementSchema received by the Topology server. Run this task on the foreground loop.
  async::Executor executor(dispatcher());
  executor.schedule_task(topology.PromiseSchema().then(
      [quit = QuitLoopClosure(), &element_name](fpromise::result<ElementSchema, void>& result) {
        EXPECT_TRUE(result.is_ok());
        ElementSchema schema = result.take_value();
        ASSERT_EQ(schema.element_name(), element_name);
        ASSERT_EQ(schema.valid_levels(), kBinaryPowerLevels);
        ASSERT_EQ(schema.initial_current_level(), kBinaryPowerLevels[0]);
        quit();
      }));
  RunLoop();
}

TEST_F(UnmanagedElementTest, SetLevelChangesTopologyCurrentLevel) {
  ScopedBackgroundLoop background;

  // Create and run the Topology server in the background.
  auto [topology_client, topology_server] = fidl::Endpoints<Topology>::Create();
  FakeTopology topology(background.dispatcher(), std::move(topology_server));

  // Create an unmanaged element and add it to the power topology.
  const std::string element_name = "unmanaged-element";
  const PowerLevel initial_level = kBinaryPowerLevels[0];
  auto element =
      UnmanagedElement::Add(topology_client, element_name, kBinaryPowerLevels, initial_level);
  EXPECT_TRUE(element.is_ok());

  // Get the CurrentLevel server end from the ElementSchema received via Topology.
  async::Executor executor(dispatcher());
  ElementSchema schema;
  executor.schedule_task(topology.PromiseSchema().then(
      [quit = QuitLoopClosure(), &schema](fpromise::result<ElementSchema, void>& result) {
        EXPECT_TRUE(result.is_ok());
        schema = result.take_value();
        quit();
      }));
  RunLoop();

  // Create a CurrentLevel server to handle Update requests in the background. DispatcherBound lets
  // us make safe async calls between the foreground (this test) and background dispatchers.
  async_patterns::DispatcherBound<FakeCurrentLevel> current_level_server(
      background.dispatcher(), std::in_place, async_patterns::PassDispatcher,
      std::move(schema.level_control_channels()->current()), initial_level);

  // Check that CurrentLevel is still set to the initial value.
  executor.schedule_task(
      current_level_server.AsyncCall(&FakeCurrentLevel::current_level)
          .promise()
          .then([quit = QuitLoopClosure(), &initial_level](fpromise::result<PowerLevel>& result) {
            EXPECT_TRUE(result.is_ok());
            ASSERT_EQ(result.value(), initial_level);
            quit();
          }));
  RunLoop();

  // Verify CurrentLevel changes after calling SetLevel on the unmanaged element.
  element->SetLevel(kBinaryPowerLevels[1]);
  executor.schedule_task(
      current_level_server.AsyncCall(&FakeCurrentLevel::current_level)
          .promise()
          .then([quit = QuitLoopClosure()](fpromise::result<PowerLevel>& result) {
            EXPECT_TRUE(result.is_ok());
            ASSERT_EQ(result.value(), kBinaryPowerLevels[1]);
            quit();
          }));
  RunLoop();
}

}  // namespace
