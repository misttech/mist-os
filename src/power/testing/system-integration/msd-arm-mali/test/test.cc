// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.gpu.magma/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/natural_ostream.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/fdio/directory.h>
#include <lib/magma/magma.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma_client/test_util/magma_map_cpu.h>
#include <lib/magma_client/test_util/test_device_helper.h>
#include <lib/zx/channel.h>
#include <poll.h>

#include <filesystem>
#include <thread>

#include <gtest/gtest.h>

#include "src/graphics/drivers/msd-arm-mali/include/magma_arm_mali_types.h"
#include "src/graphics/drivers/msd-arm-mali/include/magma_arm_mali_vendor_id.h"
#include "src/graphics/drivers/msd-arm-mali/include/magma_vendor_queries.h"
#include "src/graphics/drivers/msd-arm-mali/tests/integration/mali_utils.h"
#include "src/power/testing/system-integration/util/test_util.h"

namespace {

class TestConnection : public magma::TestDeviceBase {
 public:
  TestConnection() : magma::TestDeviceBase(MAGMA_VENDOR_ID_MALI) {
    magma_device_create_connection(device(), &connection_);
    DASSERT(connection_);

    magma_connection_create_context(connection_, &context_id_);
    helper_.emplace(connection_, context_id_);
  }

  ~TestConnection() {
    magma_connection_release_context(connection_, context_id_);

    if (connection_)
      magma_connection_release(connection_);
  }

  void SubmitCommandBuffer(mali_utils::AtomHelper::How how, uint8_t atom_number,
                           uint8_t atom_dependency, bool protected_mode) {
    helper_->SubmitCommandBuffer(how, atom_number, atom_dependency, protected_mode);
  }

 private:
  magma_connection_t connection_;
  uint32_t context_id_;
  std::optional<mali_utils::AtomHelper> helper_;
};

class PowerSystemIntegration : public system_integration_utils::TestLoopBase, public testing::Test {
 public:
  struct InspectSelectors {
    const std::string sag_moniker = "bootstrap/system-activity-governor/system-activity-governor";
    const std::vector<std::string> sag_exec_state_level = {"root", "power_elements",
                                                           "execution_state", "power_level"};
    const std::string pb_moniker = "bootstrap/power-broker";
    std::vector<std::string> mali_gpu_required_level;
    std::vector<std::string> mali_gpu_current_level;

    // Driver monikers are unstable, so wildcard the moniker and use a tree name
    const std::string msd_arm_mali_inspect_tree_name = "mali";
    const std::string msd_arm_mali_moniker = "bootstrap/*-drivers*";
    const std::vector<std::string> msd_arm_mali_power_lease_active = {
        "root", "msd-arm-mali", "device", "power_lease_active"};
    const std::vector<std::string> msd_arm_mali_required_power_level = {
        "root", "msd-arm-mali", "device", "required_power_level"};
    const std::vector<std::string> msd_arm_mali_current_power_level = {
        "root", "msd-arm-mali", "device", "current_power_level"};
  };
  void SetUp() override { Initialize(); }

  InspectSelectors GetInspectSelectors(diagnostics::reader::ArchiveReader& reader) {
    InspectSelectors selectors;

    const auto mali_gpu_element_id =
        GetPowerElementId(reader, selectors.pb_moniker, "mali-gpu-hardware");
    EXPECT_TRUE(mali_gpu_element_id.is_ok());
    selectors.mali_gpu_required_level = {"root",     "broker",
                                         "topology", "fuchsia.inspect.Graph",
                                         "topology", mali_gpu_element_id.value(),
                                         "meta",     "required_level"};
    selectors.mali_gpu_current_level = {"root",     "broker",
                                        "topology", "fuchsia.inspect.Graph",
                                        "topology", mali_gpu_element_id.value(),
                                        "meta",     "current_level"};
    return selectors;
  }
};

TEST_F(PowerSystemIntegration, SuspendResume) {
  // Duration to sleep much be << 1 second, or else the command submission may timeout.
  const auto kPollDuration = zx::msec(50);
  // To enable changing SAG's power levels, first trigger the "boot complete" logic. This is done by
  // setting both exec state level and app activity level to active.
  test_sagcontrol::SystemActivityGovernorState state = GetBootCompleteState();
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);
  ASSERT_TRUE(SetBootComplete());

  diagnostics::reader::ArchiveReader reader(dispatcher());

  const InspectSelectors s = GetInspectSelectors(reader);

  // Verify boot complete state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - no lease, powered off.
  MatchInspectData(reader, s.sag_moniker, std::nullopt, s.sag_exec_state_level,
                   uint64_t{2});  // kActive
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level,
                   uint64_t{0});  // Off
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level,
                   uint64_t{0});  // Off

  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{0});

  std::unique_ptr<TestConnection> test;
  test.reset(new TestConnection());
  std::atomic_bool finished_test{false};
  std::thread command_loop([&] {
    uint32_t i = 0;
    while (!finished_test) {
      {
        SCOPED_TRACE(std::to_string(i++));
        test->SubmitCommandBuffer(mali_utils::AtomHelper::NORMAL, 1, 0, false);
      }
    }
  });

  // - Power Broker: mali gpu powered on.
  // - msd_arm_mali - lease enabled, powered on.
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level, uint64_t{1});
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level, uint64_t{1});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, true);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{1});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{1});

  // Emulate system suspend.
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kInactive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kInactive);
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);
  ASSERT_EQ(AwaitSystemSuspend(), ZX_OK);

  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - lease enabled, powered off.
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level, uint64_t{0});
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, true);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{0});

  // Emulate system resume.
  ASSERT_EQ(StartSystemResume(), ZX_OK);
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kActive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kActive);
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);

  // - Power Broker: mali gpu powered on.
  // - msd_arm_mali - lease enabled, powered on.
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level, uint64_t{1});
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level, uint64_t{1});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, true);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{1});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{1});
  finished_test = true;
  command_loop.join();

  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - lease off, powered off.
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level, uint64_t{0});
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{0});
}

TEST_F(PowerSystemIntegration, PowerIdle) {
  auto topology_result = component::Connect<fuchsia_power_broker::Topology>();
  ASSERT_EQ(ZX_OK, topology_result.status_value());
  fidl::ClientEnd topology = std::move(*topology_result);

  // Duration to sleep much be << 1 second, or else the command submission may timeout.
  const auto kPollDuration = zx::msec(50);
  // To enable changing SAG's power levels, first trigger the "boot complete" logic. This is done by
  // setting both exec state level and app activity level to active.
  test_sagcontrol::SystemActivityGovernorState state = GetBootCompleteState();
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);
  ASSERT_TRUE(SetBootComplete());

  diagnostics::reader::ArchiveReader reader(dispatcher());
  const InspectSelectors s = GetInspectSelectors(reader);

  // Verify boot complete state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - no lease, powered off.
  MatchInspectData(reader, s.sag_moniker, std::nullopt, s.sag_exec_state_level,
                   uint64_t{2});  // kActive
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level,
                   uint64_t{0});  // Off
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level,
                   uint64_t{0});  // Off

  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{0});

  // Connect after boot complete checks to ensure that the driver has loaded already.
  fprintf(stderr, "Starting to iterate services\n");
  fidl::SyncClient<fuchsia_gpu_magma::PowerElementProvider> power_client;
  const char* kGpuServicePath = "/svc/fuchsia.gpu.magma.Service";
  for (const auto& entry : std::filesystem::directory_iterator(kGpuServicePath)) {
    fprintf(stderr, "Got instance at path %s\n", entry.path().c_str());
    std::string instance_id = entry.path().filename().native();
    auto service_result = component::OpenService<fuchsia_gpu_magma::Service>(instance_id);
    ASSERT_TRUE(service_result.is_ok());
    auto connect_result = service_result->connect_power_element_provider();
    ASSERT_TRUE(connect_result.is_ok());
    power_client = fidl::SyncClient(std::move(*connect_result));
    // Only one GPU driver should be installed in this custom board, and it should be for the ARM
    // mali.
    magma::TestDeviceBase test_device(std::string(kGpuServicePath) + "/" + instance_id + "/device");
    ASSERT_EQ(static_cast<uint64_t>(MAGMA_VENDOR_ID_MALI), test_device.GetVendorId());
  }
  ASSERT_TRUE(power_client.is_valid());

  auto power_goals = power_client->GetPowerGoals();
  ASSERT_TRUE(power_goals.is_ok()) << power_goals.error_value();

  zx::event on_ready_for_work_event;
  EXPECT_EQ(1u, power_goals->goals().size());
  for (auto& goal : power_goals->goals()) {
    ASSERT_TRUE(goal.type());
    ASSERT_EQ(*goal.type(), fuchsia_gpu_magma::PowerGoalType::kOnReadyForWork);
    ASSERT_TRUE(goal.token());
    on_ready_for_work_event = std::move(*goal.token());
  }

  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control;

  fdf_power::LeaseDependency lease_dependency{
      .levels_by_preference = {1},
      .token = std::move(on_ready_for_work_event),
      .type = fuchsia_power_broker::DependencyType::kAssertive};
  std::vector<fdf_power::LeaseDependency> lease_dependencies;
  lease_dependencies.push_back(std::move(lease_dependency));
  auto lease_helper_result = fdf_power::CreateLeaseHelper(
      topology, std::move(lease_dependencies), "mali_idle", dispatcher(),
      []() { EXPECT_TRUE(false) << "In error callback"; });
  if (!lease_helper_result.is_ok()) {
    auto [fidl_status, add_element_error] = lease_helper_result.error_value();

    ASSERT_TRUE(fidl_status.ok()) << fidl_status.FormatDescription();
    ASSERT_TRUE(add_element_error);
    ASSERT_FALSE(add_element_error) << fidl::ToUnderlying(*add_element_error);
  }

  lease_helper_result->AcquireLease(
      [&lease_control](fidl::Result<fuchsia_power_broker::Lessor::Lease>& result) {
        EXPECT_TRUE(result.is_ok());
        lease_control = std::move(result.value().lease_control());
      });

  // - Power Broker: mali gpu powered on.
  // - msd_arm_mali - lease disabled, powered on.
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level, uint64_t{1});
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level, uint64_t{1});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{1});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{1});

  lease_control.reset();

  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - lease disabled, powered off.
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_required_level,
                   uint64_t{0});  // Off
  MatchInspectData(reader, s.pb_moniker, std::nullopt, s.mali_gpu_current_level,
                   uint64_t{0});  // Off

  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, s.msd_arm_mali_moniker, s.msd_arm_mali_inspect_tree_name,
                   s.msd_arm_mali_current_power_level, uint64_t{0});
}

}  // namespace
