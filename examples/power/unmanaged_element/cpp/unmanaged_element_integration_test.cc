// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/diagnostics/reader/cpp/inspect.h>
#include <lib/fidl/cpp/client.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>

#include <gtest/gtest.h>
#include <sdk/lib/driver/power/cpp/testing/scoped_background_loop.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "examples/power/unmanaged_element/cpp/unmanaged_element.h"

namespace {

using component_testing::ChildRef;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::RealmBuilder;
using component_testing::RealmRoot;
using component_testing::Route;
using diagnostics::reader::ArchiveReader;
using diagnostics::reader::InspectData;
using examples::power::UnmanagedElement;
using fdf_power::testing::ScopedBackgroundLoop;
using fuchsia_power_broker::BinaryPowerLevel;
using fuchsia_power_broker::PowerLevel;
using fuchsia_power_broker::Status;
using fuchsia_power_broker::Topology;
using inspect::IntPropertyValue;
using inspect::NodeValue;
using inspect::StringPropertyValue;

const std::vector<PowerLevel> kBinaryPowerLevels = {fidl::ToUnderlying(BinaryPowerLevel::kOff),
                                                    fidl::ToUnderlying(BinaryPowerLevel::kOn)};

class UnmanagedElementIntegrationTest : public gtest::RealLoopFixture {};

class TestBrokerRealm {
 public:
  explicit TestBrokerRealm(async_dispatcher_t* dispatcher)
      : realm_(RealmBuilder::Create()
                   .AddChild(power_broker_.name.data(), "#meta/power-broker.cm")
                   .AddRoute(Route{.capabilities = {Protocol{"fuchsia.power.broker.Topology"}},
                                   .source = power_broker_,
                                   .targets = {ParentRef()}})
                   .Build()) {}

  template <typename Protocol>
  fidl::ClientEnd<Protocol> TakeClient() {
    zx::result<fidl::ClientEnd<Protocol>> client = realm_.component().Connect<Protocol>();
    EXPECT_TRUE(client.is_ok()) << client.error_value();
    return std::move(client.value());
  }

  std::string GetInspectMoniker() const {
    return "realm_builder\\:" + realm_.component().GetChildName() + "/" + power_broker_.name.data();
  }

 private:
  ChildRef power_broker_{"test-power-broker"};
  RealmRoot realm_;
};

void CheckCurrentLevel(fidl::ClientEnd<Status>& status, const PowerLevel& expected_level) {
  auto result = fidl::WireCall(status)->WatchPowerLevel();
  EXPECT_TRUE(result->is_ok());
  ASSERT_EQ(result->value()->current_level, expected_level);
}

TEST_F(UnmanagedElementIntegrationTest, UnmanagedElementStatePropagatesToStatusChannel) {
  ScopedBackgroundLoop background;
  TestBrokerRealm broker_realm(background.dispatcher());
  const PowerLevel initial_level = kBinaryPowerLevels[0];
  auto topology = broker_realm.TakeClient<Topology>();
  auto element =
      UnmanagedElement::Add(topology, "unmanaged-element", kBinaryPowerLevels, initial_level);

  // Use the unmanaged element's ElementControl client to open a Status channel.
  auto [status_channel, status_channel_server] = fidl::Endpoints<Status>::Create();
  auto& element_control = element->description().element_control_client;
  EXPECT_TRUE(element_control.has_value());
  EXPECT_TRUE(
      fidl::WireCall(*element_control)->OpenStatusChannel(std::move(status_channel_server)).ok());

  // Listen for updates on the Status channel.
  CheckCurrentLevel(status_channel, initial_level);

  // Change power levels on the unmanaged element and observe updates on the Status channel.
  element->SetLevel(kBinaryPowerLevels[1]);
  CheckCurrentLevel(status_channel, kBinaryPowerLevels[1]);
  element->SetLevel(kBinaryPowerLevels[0]);
  CheckCurrentLevel(status_channel, kBinaryPowerLevels[0]);
}

/// Searches the provided Inspect hierarchy for a `meta` node matching:
/// `root/broker/topology/fuchsia.inspect.Graph/topology/<ID string>/meta`.
void CheckTopologyInspectNode(const fpromise::result<std::vector<InspectData>, std::string>& result,
                              const std::string& name, const PowerLevel& expected_level) {
  EXPECT_TRUE(result.is_ok()) << result.error();
  EXPECT_EQ(result.value().size(), 1u);
  const InspectData& data = result.value().front();
  EXPECT_TRUE(data.payload().has_value());
  const inspect::Hierarchy* topology_node = data.payload().value()->GetByPath(
      {"broker", "topology", "fuchsia.inspect.Graph", "topology"});
  EXPECT_NE(topology_node, nullptr);
  for (const inspect::Hierarchy& child : topology_node->children()) {
    const inspect::Hierarchy* meta = child.GetByPath({"meta"});
    if (meta) {
      const NodeValue& meta_node = meta->node();
      const StringPropertyValue* meta_name = meta_node.get_property<StringPropertyValue>("name");
      if (meta_name != nullptr && meta_name->value() == name) {
        const IntPropertyValue* current_level =
            meta_node.get_property<IntPropertyValue>("current_level");
        EXPECT_NE(current_level, nullptr);
        ASSERT_EQ(current_level->value(), expected_level);
        return;
      }
    }
  }
  FAIL() << "Power Broker Inspect tree missing element with current_level: " << name;
}

TEST_F(UnmanagedElementIntegrationTest, UnmanagedElementStatePropagatesToPowerBrokerInspect) {
  ScopedBackgroundLoop background;
  TestBrokerRealm broker_realm(background.dispatcher());
  const PowerLevel initial_level = kBinaryPowerLevels[0];
  auto topology = broker_realm.TakeClient<Topology>();
  const std::string element_name = "unmanaged-element";
  auto element = UnmanagedElement::Add(topology, element_name, kBinaryPowerLevels, initial_level);

  async::Executor executor(dispatcher());
  ArchiveReader reader(dispatcher(), {broker_realm.GetInspectMoniker() + ":root"});

  // Wait for Inspect data to be ready.
  bool ready = false;
  while (!ready) {
    executor.schedule_task(reader.GetInspectSnapshot().then(
        [&ready](fpromise::result<std::vector<InspectData>, std::string>& result) {
          EXPECT_TRUE(result.is_ok()) << result.error();
          ready = !result.value().empty();
        }));
    RunLoopWithTimeoutOrUntil([&ready] { return ready; });
  }

  // Check initial power level in Power Broker's Inspect tree.
  executor.schedule_task(reader.GetInspectSnapshot().then(
      [&element_name,
       quit = QuitLoopClosure()](fpromise::result<std::vector<InspectData>, std::string>& result) {
        CheckTopologyInspectNode(result, element_name, kBinaryPowerLevels[0]);
        quit();
      }));
  RunLoop();

  // Change power levels on the unmanaged element and observe updates in Broker Inspect
  // tree.
  element->SetLevel(kBinaryPowerLevels[1]);
  executor.schedule_task(reader.GetInspectSnapshot().then(
      [&element_name,
       quit = QuitLoopClosure()](fpromise::result<std::vector<InspectData>, std::string>& result) {
        CheckTopologyInspectNode(result, element_name, kBinaryPowerLevels[1]);
        quit();
      }));
  RunLoop();
  element->SetLevel(kBinaryPowerLevels[0]);
  executor.schedule_task(reader.GetInspectSnapshot().then(
      [&element_name,
       quit = QuitLoopClosure()](fpromise::result<std::vector<InspectData>, std::string>& result) {
        CheckTopologyInspectNode(result, element_name, kBinaryPowerLevels[0]);
        quit();
      }));
  RunLoop();
}

}  // namespace
