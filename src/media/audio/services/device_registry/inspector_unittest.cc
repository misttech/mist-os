// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/inspector_unittest.h"

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <lib/inspect/cpp/health.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/testing/cpp/inspect.h>

#include <algorithm>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/inspector.h"

using ::inspect::BoolPropertyValue;
using ::inspect::IntPropertyValue;
using ::inspect::StringPropertyValue;
using ::inspect::UintPropertyValue;

namespace fad = fuchsia_audio_device;

namespace media_audio {

TEST_F(InspectorTest, DefaultValues) {
  auto before_get_hierarchy = zx::clock::get_monotonic();

  auto hierarchy = GetHierarchy();
  ASSERT_EQ(hierarchy.name(), "root");
  EXPECT_EQ(hierarchy.node().properties().size(), 3u);
  // Expect metrics with default values in the root node.
  EXPECT_EQ(hierarchy.node()
                .get_property<UintPropertyValue>(std::string(kDetectionConnectionErrors))
                ->value(),
            0u);
  EXPECT_EQ(
      hierarchy.node().get_property<UintPropertyValue>(std::string(kDetectionOtherErrors))->value(),
      0u);
  EXPECT_EQ(hierarchy.node()
                .get_property<UintPropertyValue>(std::string(kDetectionUnsupportedDevices))
                ->value(),
            0u);

  // Expect empty child nodes for Devices and FIDL servers (with children).
  // Expect fuchsia.inspect.Health to already be in the "starting up" state - this occurs at static
  // initialization time: when audio_device_registry's (or this unittest bin's) main() starts up.
  ASSERT_FALSE(hierarchy.children().empty());

  auto devices_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kDevices; });
  ASSERT_NE(devices_node, hierarchy.children().end());
  ASSERT_TRUE(devices_node->node().properties().empty());
  ASSERT_TRUE(devices_node->children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_EQ(fidl_servers_node->children().size(), 6u);

  auto registry_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kRegistryServerInstances; });
  EXPECT_NE(registry_servers_node, fidl_servers_node->children().end());
  EXPECT_TRUE(registry_servers_node->node().properties().empty());
  EXPECT_TRUE(registry_servers_node->children().empty());

  auto observer_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kObserverServerInstances; });
  EXPECT_NE(observer_servers_node, fidl_servers_node->children().end());
  EXPECT_TRUE(observer_servers_node->node().properties().empty());
  EXPECT_TRUE(observer_servers_node->children().empty());

  auto control_creator_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kControlCreatorServerInstances; });
  EXPECT_NE(control_creator_servers_node, fidl_servers_node->children().end());
  EXPECT_TRUE(control_creator_servers_node->node().properties().empty());
  EXPECT_TRUE(control_creator_servers_node->children().empty());

  auto control_servers_node =
      std::find_if(fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kControlServerInstances; });
  EXPECT_NE(control_servers_node, fidl_servers_node->children().end());
  EXPECT_TRUE(control_servers_node->node().properties().empty());
  EXPECT_TRUE(control_servers_node->children().empty());

  auto ring_buffer_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kRingBufferServerInstances; });
  EXPECT_NE(ring_buffer_servers_node, fidl_servers_node->children().end());
  EXPECT_TRUE(ring_buffer_servers_node->node().properties().empty());
  EXPECT_TRUE(ring_buffer_servers_node->children().empty());

  auto provider_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kProviderServerInstances; });
  EXPECT_NE(provider_servers_node, fidl_servers_node->children().end());
  EXPECT_TRUE(provider_servers_node->node().properties().empty());
  EXPECT_TRUE(provider_servers_node->children().empty());

  auto health_node = std::find_if(
      hierarchy.children().begin(), hierarchy.children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == inspect::kHealthNodeName; });
  ASSERT_NE(health_node, hierarchy.children().end());
  EXPECT_EQ(health_node->node().properties().size(), 2u);
  EXPECT_EQ(health_node->node().get_property<StringPropertyValue>("status")->value(),
            inspect::kHealthStartingUp);
  EXPECT_LT(health_node->node().get_property<IntPropertyValue>(inspect::kStartTimestamp)->value(),
            before_get_hierarchy.get());
  EXPECT_TRUE(health_node->children().empty());
}

// The relevant fields are `start_timestamp_nanos at` and `status` -- located at
// root/fuchsia.inspect.Health/
TEST_F(InspectorTest, ComponentHealthy) {
  auto before_health_ok = zx::clock::get_monotonic();
  Inspector::Singleton()->RecordHealthOk();

  auto hierarchy = GetHierarchy();
  auto health_node = std::find_if(
      hierarchy.children().begin(), hierarchy.children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == inspect::kHealthNodeName; });
  ASSERT_NE(health_node, hierarchy.children().end());
  EXPECT_EQ(health_node->node().properties().size(), 2u);
  EXPECT_EQ(health_node->node().get_property<StringPropertyValue>("status")->value(),
            inspect::kHealthOk);
  EXPECT_LT(health_node->node().get_property<IntPropertyValue>(inspect::kStartTimestamp)->value(),
            before_health_ok.get());
  EXPECT_TRUE(health_node->children().empty());
}

// The relevant fields are `start_timestamp_nanos at`, `status` and `message` -- located at
// root/fuchsia.inspect.Health/
TEST_F(InspectorTest, ComponentUnhealthy) {
  constexpr std::string kUnhealthyMessasge{"Unhealthy message"};

  auto before_unhealthy = zx::clock::get_monotonic();
  Inspector::Singleton()->RecordUnhealthy(kUnhealthyMessasge);

  auto hierarchy = GetHierarchy();
  auto health_node = std::find_if(
      hierarchy.children().begin(), hierarchy.children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == inspect::kHealthNodeName; });
  ASSERT_NE(health_node, hierarchy.children().end());
  EXPECT_EQ(health_node->node().properties().size(), 3u);
  EXPECT_EQ(health_node->node().get_property<StringPropertyValue>("status")->value(),
            inspect::kHealthUnhealthy);
  EXPECT_LT(health_node->node().get_property<IntPropertyValue>(inspect::kStartTimestamp)->value(),
            before_unhealthy.get());
  EXPECT_EQ(health_node->node().get_property<StringPropertyValue>("message")->value(),
            kUnhealthyMessasge);
  EXPECT_TRUE(health_node->children().empty());
}

// The relevant fields are `added at`, `token id` and many others -- located at
// root/Devices/[device name]/
TEST_F(InspectorTest, DetectedDevice) {
  auto before_add = zx::clock::get_monotonic();
  auto fake_driver = CreateAndAddFakeComposite();

  auto hierarchy = GetHierarchy();
  ASSERT_EQ(hierarchy.name(), "root");
  EXPECT_EQ(hierarchy.node().properties().size(), 3u);
  EXPECT_EQ(hierarchy.node()
                .get_property<UintPropertyValue>(std::string(kDetectionConnectionErrors))
                ->value(),
            0u);
  EXPECT_EQ(
      hierarchy.node().get_property<UintPropertyValue>(std::string(kDetectionOtherErrors))->value(),
      0u);
  EXPECT_EQ(hierarchy.node()
                .get_property<UintPropertyValue>(std::string(kDetectionUnsupportedDevices))
                ->value(),
            0u);

  auto devices_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kDevices; });
  ASSERT_NE(devices_node, hierarchy.children().end());
  ASSERT_TRUE(devices_node->node().properties().empty());
  ASSERT_EQ(devices_node->children().size(), 1u);

  auto device_node = devices_node->children().cbegin();
  EXPECT_GE(device_node->node().properties().size(), 8u);
  EXPECT_GT(device_node->node().get_property<IntPropertyValue>(std::string(kAddedAt))->value(),
            before_add.get());
  EXPECT_EQ(device_node->node().get_property<UintPropertyValue>(std::string(kTokenId))->value(),
            0u);
  EXPECT_EQ(device_node->node().get_property<UintPropertyValue>(std::string(kClockDomain))->value(),
            FakeComposite::kDefaultClockDomain);
  EXPECT_EQ(device_node->node().get_property<BoolPropertyValue>(std::string(kHealthy))->value(),
            true);
  EXPECT_EQ(
      device_node->node().get_property<StringPropertyValue>(std::string(kManufacturer))->value(),
      FakeComposite::kDefaultManufacturer);
  EXPECT_EQ(device_node->node().get_property<StringPropertyValue>(std::string(kProduct))->value(),
            FakeComposite::kDefaultProduct);
  EXPECT_EQ(
      device_node->node().get_property<StringPropertyValue>(std::string(kDeviceType))->value(),
      "COMPOSITE");
  EXPECT_EQ(device_node->node().get_property<StringPropertyValue>(std::string(kUniqueId))->value(),
            UidToString(FakeComposite::kDefaultUniqueInstanceId));
  ASSERT_EQ(device_node->children().size(), 1u);

  auto ring_buffer_elements_node =
      std::find_if(device_node->children().begin(), device_node->children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kRingBufferElements; });
  ASSERT_NE(ring_buffer_elements_node, device_node->children().end());
  EXPECT_TRUE(ring_buffer_elements_node->node().properties().empty());
  ASSERT_EQ(ring_buffer_elements_node->children().size(), 2u);

  auto first_rb_element_id = ring_buffer_elements_node->children()
                                 .cbegin()
                                 ->node()
                                 .get_property<UintPropertyValue>(std::string(kElementId))
                                 ->value();
  auto last_rb_element_id = ring_buffer_elements_node->children()
                                .crbegin()
                                ->node()
                                .get_property<UintPropertyValue>(std::string(kElementId))
                                ->value();
  EXPECT_TRUE((first_rb_element_id == FakeComposite::kSourceRbElementId &&
               last_rb_element_id == FakeComposite::kDestRbElementId) ||
              (first_rb_element_id == FakeComposite::kDestRbElementId &&
               last_rb_element_id == FakeComposite::kSourceRbElementId));
  EXPECT_TRUE(ring_buffer_elements_node->children().cbegin()->children().empty());
  EXPECT_TRUE(ring_buffer_elements_node->children().crbegin()->children().empty());
}

// The relevant field is `removed at` -- located at // root/Devices/[device name]/
TEST_F(InspectorTest, RemovedDevice) {
  auto before_add = zx::clock::get_monotonic();
  auto fake_driver = CreateAndAddFakeStreamConfigOutput();

  auto before_drop = zx::clock::get_monotonic();
  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  auto hierarchy = GetHierarchy();
  ASSERT_EQ(hierarchy.name(), "root");
  EXPECT_EQ(hierarchy.node().properties().size(), 3u);
  EXPECT_EQ(hierarchy.node()
                .get_property<UintPropertyValue>(std::string(kDetectionConnectionErrors))
                ->value(),
            0u);
  EXPECT_EQ(
      hierarchy.node().get_property<UintPropertyValue>(std::string(kDetectionOtherErrors))->value(),
      0u);
  EXPECT_EQ(hierarchy.node()
                .get_property<UintPropertyValue>(std::string(kDetectionUnsupportedDevices))
                ->value(),
            0u);

  auto devices_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kDevices; });
  ASSERT_NE(devices_node, hierarchy.children().end());
  ASSERT_TRUE(devices_node->node().properties().empty());
  ASSERT_FALSE(devices_node->children().empty());

  auto device_node = devices_node->children().cbegin();
  EXPECT_GT(device_node->node().properties().size(), 3u);
  EXPECT_GT(device_node->node().get_property<IntPropertyValue>(std::string(kAddedAt))->value(),
            before_add.get());
  // We couldn't check kIsInput earlier but can now: StreamConfig has in/out
  EXPECT_EQ(device_node->node().get_property<BoolPropertyValue>(std::string(kIsInput))->value(),
            false);
  EXPECT_GT(device_node->node().get_property<IntPropertyValue>(std::string(kRemovedAt))->value(),
            before_drop.get());
  EXPECT_EQ(device_node->children().size(), 1u);
}

// The relevant fields are `created at` and `destroyed at` -- located at
// root/FIDL servers/RegistryServer instances/0/
// We don't test kDestroyedAt because of unpredictable cleanup timing.
TEST_F(InspectorTest, CreateRegistryServer) {
  auto before_create = zx::clock::get_monotonic();
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto registry_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kRegistryServerInstances; });
  ASSERT_NE(registry_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(registry_servers_node->node().properties().empty());
  ASSERT_FALSE(registry_servers_node->children().empty());

  auto registry_server_node = registry_servers_node->children().cbegin();
  EXPECT_EQ(registry_server_node->name(), "0");
  EXPECT_EQ(registry_server_node->node().properties().size(), 1u);
  EXPECT_GT(
      registry_server_node->node().get_property<IntPropertyValue>(std::string(kCreatedAt))->value(),
      before_create.get());
  EXPECT_TRUE(registry_server_node->children().empty());
}

// The relevant fields are `created at` and `destroyed at` -- located at
// root/FIDL servers/ObserverServer instances/0/
// We don't test kDestroyedAt because of unpredictable cleanup timing.
TEST_F(InspectorTest, CreateObserverServer) {
  auto fake_driver = CreateAndAddFakeStreamConfigOutput();
  auto registry = CreateTestRegistryServer();
  std::optional<TokenId> added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_TRUE(added_device_id.has_value());

  auto before_create = zx::clock::get_monotonic();
  auto observer = CreateTestObserverServer(*adr_service()->devices().begin());
  ASSERT_EQ(ObserverServer::count(), 1u);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto observer_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kObserverServerInstances; });
  ASSERT_NE(observer_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(observer_servers_node->node().properties().empty());
  ASSERT_FALSE(observer_servers_node->children().empty());

  auto observer_server_node = observer_servers_node->children().cbegin();
  EXPECT_EQ(observer_server_node->name(), "0");
  EXPECT_EQ(observer_server_node->node().properties().size(), 1u);
  EXPECT_GT(
      observer_server_node->node().get_property<IntPropertyValue>(std::string(kCreatedAt))->value(),
      before_create.get());
  EXPECT_TRUE(observer_server_node->children().empty());
}

// The relevant fields are `created at` and `destroyed at` -- located at
// root/FIDL servers/ControlCreatorServer instances/0/
// We don't test kDestroyedAt because of unpredictable cleanup timing.
TEST_F(InspectorTest, CreateControlCreatorServer) {
  auto before_create = zx::clock::get_monotonic();
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto control_creator_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kControlCreatorServerInstances; });
  ASSERT_NE(control_creator_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(control_creator_servers_node->node().properties().empty());
  ASSERT_FALSE(control_creator_servers_node->children().empty());

  auto control_creator_server_node = control_creator_servers_node->children().cbegin();
  EXPECT_EQ(control_creator_server_node->name(), "0");
  EXPECT_EQ(control_creator_server_node->node().properties().size(), 1u);
  EXPECT_GT(control_creator_server_node->node()
                .get_property<IntPropertyValue>(std::string(kCreatedAt))
                ->value(),
            before_create.get());
  EXPECT_TRUE(control_creator_server_node->children().empty());
}

// The relevant fields are `created at` and `destroyed at` -- located at
// root/FIDL servers/ControlServer instances/0/
// We don't test kDestroyedAt because of unpredictable cleanup timing.
TEST_F(InspectorTest, CreateControlServer) {
  auto fake_driver = CreateAndAddFakeStreamConfigOutput();
  auto registry = CreateTestRegistryServer();
  std::optional<TokenId> added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_TRUE(added_device_id.has_value());

  auto [presence, device_to_control] = adr_service()->FindDeviceByTokenId(*added_device_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto before_create = zx::clock::get_monotonic();
  auto control = CreateTestControlServer(device_to_control);
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto control_servers_node =
      std::find_if(fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kControlServerInstances; });
  ASSERT_NE(control_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(control_servers_node->node().properties().empty());
  ASSERT_FALSE(control_servers_node->children().empty());

  auto control_server_node = control_servers_node->children().cbegin();
  EXPECT_EQ(control_server_node->name(), "0");
  EXPECT_EQ(control_server_node->node().properties().size(), 1u);
  EXPECT_GT(
      control_server_node->node().get_property<IntPropertyValue>(std::string(kCreatedAt))->value(),
      before_create.get());
  EXPECT_TRUE(control_server_node->children().empty());
}

// The relevant fields are `created at` and `destroyed at` -- located at
// root/FIDL servers/RingBufferServer instances/0/
// We don't test kDestroyedAt because of unpredictable cleanup timing.
TEST_F(InspectorTest, CreateRingBufferServer) {
  // start of RingBuffer testcase setup, same as other RingBuffer unittests
  auto fake_driver = CreateFakeStreamConfigOutput();
  fake_driver->AllocateRingBuffer(8192);
  auto device =
      Device::Create(adr_service(), dispatcher(), "Test output name", fad::DeviceType::kOutput,
                     fad::DriverClient::WithStreamConfig(fake_driver->Enable()));
  adr_service()->AddDevice(device);
  RunLoopUntilIdle();
  auto registry = CreateTestRegistryServer();
  std::optional<TokenId> added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_TRUE(added_device_id.has_value());
  auto [presence, device_to_control] = adr_service()->FindDeviceByTokenId(*added_device_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fad::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fad::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
  auto before_create = zx::clock::get_monotonic();
  auto ring_buffer = adr_service()->CreateRingBufferServer(std::move(ring_buffer_server_end),
                                                           control->server_ptr(), device_to_control,
                                                           fad::kDefaultRingBufferElementId);
  RunLoopUntilIdle();
  EXPECT_TRUE(ring_buffer_client.is_valid());
  // end of RingBuffer testcase setup, same as other RingBuffer unittests

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto ring_buffer_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kRingBufferServerInstances; });
  ASSERT_NE(ring_buffer_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(ring_buffer_servers_node->node().properties().empty());
  ASSERT_FALSE(ring_buffer_servers_node->children().empty());

  auto ring_buffer_server_node = ring_buffer_servers_node->children().cbegin();
  EXPECT_EQ(ring_buffer_server_node->name(), "0");
  EXPECT_EQ(ring_buffer_server_node->node().properties().size(), 1u);
  EXPECT_GT(ring_buffer_server_node->node()
                .get_property<IntPropertyValue>(std::string(kCreatedAt))
                ->value(),
            before_create.get());
  EXPECT_TRUE(ring_buffer_server_node->children().empty());
}

// The relevant fields are `created at` and `destroyed at` -- located at
// root/FIDL servers/ProviderServer instances/0/
// We don't test kDestroyedAt because of unpredictable cleanup timing.
TEST_F(InspectorTest, CreateProviderServer) {
  auto before_create = zx::clock::get_monotonic();
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto provider_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kProviderServerInstances; });
  ASSERT_NE(provider_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(provider_servers_node->node().properties().empty());
  ASSERT_FALSE(provider_servers_node->children().empty());

  auto provider_node = provider_servers_node->children().cbegin();
  ASSERT_EQ(provider_node->name(), "0");
  ASSERT_EQ(provider_node->node().properties().size(), 1u);
  ASSERT_GT(provider_node->node().get_property<IntPropertyValue>(std::string(kCreatedAt))->value(),
            before_create.get());
  ASSERT_EQ(provider_node->children().size(), 1u);

  auto added_devices_node = provider_node->children().cbegin();
  EXPECT_EQ(added_devices_node->name(), std::string(kAddedDevices));
  EXPECT_TRUE(added_devices_node->node().properties().empty());
  EXPECT_TRUE(added_devices_node->children().empty());
}

// Verify that multiple instances are tracked separately: check instance name and `created at`.
TEST_F(InspectorTest, CreateMultipleServerInstances) {
  auto registry0 = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto after_create0 = zx::clock::get_monotonic();

  auto registry1 = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 2u);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto registry_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kRegistryServerInstances; });
  ASSERT_NE(registry_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(registry_servers_node->node().properties().empty());
  ASSERT_FALSE(registry_servers_node->children().empty());

  auto registry_server0_node = registry_servers_node->children().cbegin();
  ASSERT_EQ(registry_server0_node->name(), "0");
  ASSERT_EQ(registry_server0_node->node().properties().size(), 1u);
  ASSERT_LT(registry_server0_node->node()
                .get_property<IntPropertyValue>(std::string(kCreatedAt))
                ->value(),
            after_create0.get());
  ASSERT_TRUE(registry_server0_node->children().empty());

  auto registry_server1_node = registry_servers_node->children().crbegin();
  ASSERT_EQ(registry_server1_node->name(), "1");
  ASSERT_EQ(registry_server1_node->node().properties().size(), 1u);
  ASSERT_GT(registry_server1_node->node()
                .get_property<IntPropertyValue>(std::string(kCreatedAt))
                ->value(),
            after_create0.get());
  ASSERT_TRUE(registry_server1_node->children().empty());
}

// The relevant fields are `added at` and `type` (as well as [device name]) -- located at
// root/FIDL servers/ProviderServer instances/0/Added devices/[device name]/
// We add two devices, to validate that these can be tracked separately.
TEST_F(InspectorTest, ProviderAddedDevice) {
  auto before_create = zx::clock::get_monotonic();
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);

  auto fake_codec = CreateFakeCodecInput();
  auto received_callback = false;
  auto before_add_codec = zx::clock::get_monotonic();
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fad::DeviceType::kCodec,
          .driver_client = fad::DriverClient::WithCodec(fake_codec->Enable()),
      }})
      .Then([&received_callback](fidl::Result<fad::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);

  auto fake_composite = CreateFakeComposite();
  received_callback = false;
  auto before_add_composite = zx::clock::get_monotonic();
  provider->client()
      ->AddDevice({{
          .device_name = "Test composite",
          .device_type = fad::DeviceType::kComposite,
          .driver_client = fad::DriverClient::WithComposite(fake_composite->Enable()),
      }})
      .Then([&received_callback](fidl::Result<fad::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto fidl_servers_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kFidlServers; });
  ASSERT_NE(fidl_servers_node, hierarchy.children().end());
  ASSERT_TRUE(fidl_servers_node->node().properties().empty());
  ASSERT_FALSE(fidl_servers_node->children().empty());

  auto provider_servers_node = std::find_if(
      fidl_servers_node->children().begin(), fidl_servers_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kProviderServerInstances; });
  ASSERT_NE(provider_servers_node, fidl_servers_node->children().end());
  ASSERT_TRUE(provider_servers_node->node().properties().empty());
  ASSERT_FALSE(provider_servers_node->children().empty());

  auto provider_server_node = provider_servers_node->children().begin();
  ASSERT_EQ(provider_server_node->name(), "0");
  ASSERT_EQ(provider_server_node->node().properties().size(), 1u);
  ASSERT_GT(
      provider_server_node->node().get_property<IntPropertyValue>(std::string(kCreatedAt))->value(),
      before_create.get());
  ASSERT_FALSE(provider_server_node->children().empty());

  auto added_devices_node =
      std::find_if(provider_server_node->children().begin(), provider_server_node->children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kAddedDevices; });
  ASSERT_NE(added_devices_node, provider_server_node->children().end());
  ASSERT_TRUE(added_devices_node->node().properties().empty());
  ASSERT_FALSE(added_devices_node->children().empty());

  auto first_device = added_devices_node->children().cbegin();
  EXPECT_EQ(first_device->name(), "Test codec");
  EXPECT_EQ(first_device->node().properties().size(), 2u);
  EXPECT_GT(first_device->node().get_property<IntPropertyValue>(std::string(kAddedAt))->value(),
            before_add_codec.get());
  EXPECT_EQ(
      first_device->node().get_property<StringPropertyValue>(std::string(kDeviceType))->value(),
      "CODEC");
  EXPECT_TRUE(first_device->children().empty());

  auto last_device = added_devices_node->children().crbegin();
  EXPECT_EQ(last_device->name(), "Test composite");
  EXPECT_EQ(last_device->node().properties().size(), 2u);
  EXPECT_EQ(
      last_device->node().get_property<StringPropertyValue>(std::string(kDeviceType))->value(),
      "COMPOSITE");
  EXPECT_GT(last_device->node().get_property<IntPropertyValue>(std::string(kAddedAt))->value(),
            before_add_composite.get());
  EXPECT_TRUE(last_device->children().empty());
}

// The relevant fields are `started at` and `stopped at` -- located at
// root/Devices/[device-name]/ring buffer elements/0/instance 0/running intervals/0/
// We test multiple start/stop calls, to validate running intervals are tracked separately.
TEST_F(InspectorTest, StartStop) {
  // start of RingBuffer testcase setup, same as other RingBuffer unittests
  auto fake_driver = CreateFakeStreamConfigOutput();
  fake_driver->AllocateRingBuffer(8192);
  auto device =
      Device::Create(adr_service(), dispatcher(), "Test output name", fad::DeviceType::kOutput,
                     fad::DriverClient::WithStreamConfig(fake_driver->Enable()));
  adr_service()->AddDevice(device);
  RunLoopUntilIdle();
  auto registry = CreateTestRegistryServer();
  std::optional<TokenId> added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_TRUE(added_device_id.has_value());
  auto [presence, device_to_control] = adr_service()->FindDeviceByTokenId(*added_device_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fad::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fad::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<fad::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(ring_buffer_client.is_valid());
  // end of RingBuffer testcase setup, same as other RingBuffer unittests

  zx::time start_time0;
  received_callback = false;
  auto before_start0 = zx::clock::get_monotonic();
  ring_buffer_client->Start({}).Then(
      [&received_callback, &start_time0](fidl::Result<fad::RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time());
        start_time0 = zx::time(*result->start_time());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(fake_driver->started());
  EXPECT_EQ(start_time0, fake_driver->mono_start_time());
  EXPECT_GT(start_time0.get(), before_start0.get());

  auto before_stop0 = zx::clock::get_monotonic();
  received_callback = false;
  ring_buffer_client->Stop({}).Then(
      [&received_callback](fidl::Result<fad::RingBuffer::Stop>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->started());
  auto after_stop0 = zx::clock::get_monotonic();

  // Now we do another start/stop, to validate multiple `running intervals`.
  zx::time start_time1;
  received_callback = false;
  auto before_start1 = zx::clock::get_monotonic();
  ring_buffer_client->Start({}).Then(
      [&received_callback, &start_time1](fidl::Result<fad::RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time());
        start_time1 = zx::time(*result->start_time());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(fake_driver->started());
  EXPECT_EQ(start_time1, fake_driver->mono_start_time());
  EXPECT_GT(start_time1.get(), before_start1.get());

  auto before_stop1 = zx::clock::get_monotonic();
  received_callback = false;
  ring_buffer_client->Stop({}).Then(
      [&received_callback](fidl::Result<fad::RingBuffer::Stop>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->started());
  auto after_stop1 = zx::clock::get_monotonic();

  auto hierarchy = GetHierarchy();
  ASSERT_FALSE(hierarchy.children().empty());

  auto devices_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kDevices; });
  ASSERT_NE(devices_node, hierarchy.children().end());
  ASSERT_FALSE(devices_node->children().empty());

  auto device_node = devices_node->children().begin();
  auto ring_buffer_elements_node =
      std::find_if(device_node->children().begin(), device_node->children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kRingBufferElements; });
  ASSERT_NE(ring_buffer_elements_node, device_node->children().end());
  ASSERT_FALSE(ring_buffer_elements_node->children().empty());

  auto ring_buffer_element_node = ring_buffer_elements_node->children().begin();
  ASSERT_EQ(ring_buffer_element_node->name(), "0");
  ASSERT_EQ(ring_buffer_element_node->node()
                .get_property<UintPropertyValue>(std::string(kElementId))
                ->value(),
            fad::kDefaultRingBufferElementId);
  ASSERT_FALSE(ring_buffer_element_node->children().empty());

  auto ring_buffer_instance_node = ring_buffer_element_node->children().begin();
  ASSERT_EQ(ring_buffer_instance_node->name(), "instance 0");
  ASSERT_FALSE(ring_buffer_instance_node->children().empty());

  auto running_intervals = std::find_if(
      ring_buffer_instance_node->children().begin(), ring_buffer_instance_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kRunningIntervals; });
  ASSERT_NE(running_intervals, ring_buffer_instance_node->children().end());
  EXPECT_TRUE(running_intervals->node().properties().empty());
  ASSERT_EQ(running_intervals->children().size(), 2u);

  auto& first_start_stop = running_intervals->children().cbegin()->node();
  auto& last_start_stop = running_intervals->children().crbegin()->node();

  EXPECT_EQ(first_start_stop.name(), "0");
  EXPECT_EQ(first_start_stop.properties().size(), 2u);
  EXPECT_GT(first_start_stop.get_property<IntPropertyValue>(std::string(kStartedAt))->value(),
            before_start0.get());
  EXPECT_LT(first_start_stop.get_property<IntPropertyValue>(std::string(kStartedAt))->value(),
            before_stop0.get());
  EXPECT_GT(first_start_stop.get_property<IntPropertyValue>(std::string(kStoppedAt))->value(),
            before_stop0.get());
  EXPECT_LT(first_start_stop.get_property<IntPropertyValue>(std::string(kStoppedAt))->value(),
            after_stop0.get());
  EXPECT_TRUE(running_intervals->children().cbegin()->children().empty());

  EXPECT_EQ(last_start_stop.name(), "1");
  EXPECT_EQ(last_start_stop.properties().size(), 2u);
  EXPECT_GT(last_start_stop.get_property<IntPropertyValue>(std::string(kStartedAt))->value(),
            before_start1.get());
  EXPECT_LT(last_start_stop.get_property<IntPropertyValue>(std::string(kStartedAt))->value(),
            before_stop1.get());
  EXPECT_GT(last_start_stop.get_property<IntPropertyValue>(std::string(kStoppedAt))->value(),
            before_stop1.get());
  EXPECT_LT(last_start_stop.get_property<IntPropertyValue>(std::string(kStoppedAt))->value(),
            after_stop1.get());
  EXPECT_TRUE(running_intervals->children().crbegin()->children().empty());
}

// The relevant fields are `called at`, `completed at` and `channel bitmask` -- located at
// root/Devices/[device-name]/ring buffer elements/0/instance 0/SetActiveChannels calls/0/
// We test multiple SetActiveChannels calls to ensure these are tracked separately.
TEST_F(InspectorTest, SetActiveChannels) {
  // start of RingBuffer testcase setup, same as other RingBuffer unittests
  auto fake_driver = CreateFakeStreamConfigOutput();
  fake_driver->AllocateRingBuffer(8192);
  auto device =
      Device::Create(adr_service(), dispatcher(), "Test output name", fad::DeviceType::kOutput,
                     fad::DriverClient::WithStreamConfig(fake_driver->Enable()));
  adr_service()->AddDevice(device);
  RunLoopUntilIdle();
  auto registry = CreateTestRegistryServer();
  std::optional<TokenId> added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_TRUE(added_device_id.has_value());
  auto [presence, device_to_control] = adr_service()->FindDeviceByTokenId(*added_device_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fad::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fad::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<fad::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(ring_buffer_client.is_valid());
  // end of RingBuffer testcase setup, same as other RingBuffer unittests

  received_callback = false;
  zx::time set_active_channels_completed_at0;
  auto before_set_active_channels0 = zx::clock::get_monotonic();
  ring_buffer_client
      ->SetActiveChannels({{
          .channel_bitmask = 0x0,
      }})
      .Then([&received_callback, &set_active_channels_completed_at0](
                fidl::Result<fad::RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->set_time().has_value());
        set_active_channels_completed_at0 = zx::time(*result->set_time());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_GT(set_active_channels_completed_at0.get(), before_set_active_channels0.get());

  received_callback = false;
  zx::time set_active_channels_completed_at1;
  auto before_set_active_channels1 = zx::clock::get_monotonic();
  ring_buffer_client
      ->SetActiveChannels({{
          .channel_bitmask = 0x1,
      }})
      .Then([&received_callback, &set_active_channels_completed_at1](
                fidl::Result<fad::RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->set_time().has_value());
        set_active_channels_completed_at1 = zx::time(*result->set_time());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_GT(set_active_channels_completed_at1.get(), before_set_active_channels1.get());

  auto hierarchy = GetHierarchy();
  auto devices_node =
      std::find_if(hierarchy.children().begin(), hierarchy.children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kDevices; });
  ASSERT_NE(devices_node, hierarchy.children().end());
  ASSERT_FALSE(devices_node->children().empty());

  auto device_node = devices_node->children().begin();
  auto ring_buffer_elements_node =
      std::find_if(device_node->children().begin(), device_node->children().end(),
                   [](const inspect::Hierarchy& h) { return h.name() == kRingBufferElements; });
  ASSERT_NE(ring_buffer_elements_node, device_node->children().end());
  ASSERT_FALSE(ring_buffer_elements_node->children().empty());

  auto ring_buffer_element_node = ring_buffer_elements_node->children().begin();
  ASSERT_EQ(ring_buffer_element_node->name(), "0");
  ASSERT_EQ(ring_buffer_element_node->node().properties().size(), 1u);
  ASSERT_EQ(ring_buffer_element_node->node()
                .get_property<UintPropertyValue>(std::string(kElementId))
                ->value(),
            fad::kDefaultRingBufferElementId);
  ASSERT_FALSE(ring_buffer_element_node->children().empty());

  auto ring_buffer_instance_node = ring_buffer_element_node->children().begin();
  ASSERT_EQ(ring_buffer_instance_node->name(), "instance 0");
  ASSERT_FALSE(ring_buffer_instance_node->children().empty());

  auto set_active_channels_calls_node = std::find_if(
      ring_buffer_instance_node->children().begin(), ring_buffer_instance_node->children().end(),
      [](const inspect::Hierarchy& h) { return h.name() == kSetActiveChannelsCalls; });
  ASSERT_NE(set_active_channels_calls_node, ring_buffer_instance_node->children().end());
  EXPECT_TRUE(set_active_channels_calls_node->node().properties().empty());
  ASSERT_FALSE(set_active_channels_calls_node->children().empty());
  ASSERT_EQ(set_active_channels_calls_node->children().size(), 2u);

  auto& first_set_call = set_active_channels_calls_node->children().cbegin()->node();
  auto& last_set_call = set_active_channels_calls_node->children().crbegin()->node();

  EXPECT_EQ(first_set_call.name(), "0");
  EXPECT_EQ(first_set_call.properties().size(), 3u);
  EXPECT_GT(first_set_call.get_property<IntPropertyValue>(std::string(kCalledAt))->value(),
            before_set_active_channels0.get());
  EXPECT_EQ(first_set_call.get_property<IntPropertyValue>(std::string(kCompletedAt))->value(),
            set_active_channels_completed_at0.get());
  EXPECT_EQ(first_set_call.get_property<UintPropertyValue>(std::string(kChannelBitmask))->value(),
            0ull);
  EXPECT_TRUE(set_active_channels_calls_node->children().cbegin()->children().empty());

  EXPECT_EQ(last_set_call.name(), "1");
  EXPECT_EQ(last_set_call.properties().size(), 3u);
  EXPECT_GT(last_set_call.get_property<IntPropertyValue>(std::string(kCalledAt))->value(),
            before_set_active_channels1.get());
  EXPECT_EQ(last_set_call.get_property<IntPropertyValue>(std::string(kCompletedAt))->value(),
            set_active_channels_completed_at1.get());
  EXPECT_EQ(last_set_call.get_property<UintPropertyValue>(std::string(kChannelBitmask))->value(),
            1ull);
  EXPECT_TRUE(set_active_channels_calls_node->children().crbegin()->children().empty());
}

}  // namespace media_audio
