// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/natural_ostream.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/fdio/directory.h>
#include <lib/magma/magma.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma_client/test_util/magma_map_cpu.h>
#include <lib/magma_client/test_util/test_device_helper.h>
#include <lib/zx/channel.h>
#include <poll.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/graphics/drivers/msd-arm-mali/include/magma_arm_mali_types.h"
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
  void SetUp() override { Initialize(); }
};

TEST_F(PowerSystemIntegration, SuspendResume) {
  // Duration to sleep much be << 1 second, or else the command submission may timeout.
  const auto kPollDuration = zx::msec(50);
  // To enable changing SAG's power levels, first trigger the "boot complete" logic. This is done by
  // setting both exec state level and app activity level to active.
  test_sagcontrol::SystemActivityGovernorState state = GetBootCompleteState();
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);

  auto result = component::Connect<fuchsia_diagnostics::ArchiveAccessor>();
  ASSERT_EQ(ZX_OK, result.status_value());
  diagnostics::reader::ArchiveReader reader(dispatcher());

  const std::string sag_moniker = "bootstrap/system-activity-governor/system-activity-governor";
  const std::vector<std::string> sag_exec_state_level = {"root", "power_elements",
                                                         "execution_state", "power_level"};
  const std::string pb_moniker = "bootstrap/power-broker";
  const auto mali_gpu_element_id = GetPowerElementId(reader, pb_moniker, "mali-gpu-hardware");
  ASSERT_TRUE(mali_gpu_element_id.is_ok());
  const std::vector<std::string> mali_gpu_required_level = {"root",     "broker",
                                                            "topology", "fuchsia.inspect.Graph",
                                                            "topology", mali_gpu_element_id.value(),
                                                            "meta",     "required_level"};
  const std::vector<std::string> mali_gpu_current_level = {"root",     "broker",
                                                           "topology", "fuchsia.inspect.Graph",
                                                           "topology", mali_gpu_element_id.value(),
                                                           "meta",     "current_level"};

  // All drivers expose their Inspect under driver manager
  const std::string msd_arm_mali_inspect_tree_name = "mali";
  const std::string msd_arm_mali_moniker = "bootstrap/driver_manager";
  const std::vector<std::string> msd_arm_mali_power_lease_active = {"root", "msd-arm-mali",
                                                                    "device", "power_lease_active"};
  const std::vector<std::string> msd_arm_mali_required_power_level = {
      "root", "msd-arm-mali", "device", "required_power_level"};
  const std::vector<std::string> msd_arm_mali_current_power_level = {
      "root", "msd-arm-mali", "device", "current_power_level"};

  // Verify boot complete state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - no lease, powered off.
  MatchInspectData(reader, sag_moniker, std::nullopt, sag_exec_state_level,
                   uint64_t{2});  // kActive
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_required_level, uint64_t{0});  // Off
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_current_level, uint64_t{0});   // Off

  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_current_power_level, uint64_t{0});

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
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_required_level, uint64_t{1});
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_current_level, uint64_t{1});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_power_lease_active, true);
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_required_power_level, uint64_t{1});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_current_power_level, uint64_t{1});

  // Emulate system suspend.
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kInactive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kInactive);
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);

  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - lease enabled, powered off.
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_required_level, uint64_t{0});
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_current_level, uint64_t{0});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_power_lease_active, true);
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_current_power_level, uint64_t{0});

  // Emulate system resume.
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kActive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kActive);
  ASSERT_EQ(ChangeSagState(state, kPollDuration), ZX_OK);

  // - Power Broker: mali gpu powered on.
  // - msd_arm_mali - lease enabled, powered on.
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_required_level, uint64_t{1});
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_current_level, uint64_t{1});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_power_lease_active, true);
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_required_power_level, uint64_t{1});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_current_power_level, uint64_t{1});
  finished_test = true;
  command_loop.join();

  // - Power Broker: mali gpu powered off.
  // - msd_arm_mali - lease off, powered off.
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_required_level, uint64_t{0});
  MatchInspectData(reader, pb_moniker, std::nullopt, mali_gpu_current_level, uint64_t{0});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_power_lease_active, false);
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_required_power_level, uint64_t{0});
  MatchInspectData(reader, msd_arm_mali_moniker, msd_arm_mali_inspect_tree_name,
                   msd_arm_mali_current_power_level, uint64_t{0});
}
}  // namespace
