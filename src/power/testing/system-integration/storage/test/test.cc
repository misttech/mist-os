// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/natural_ostream.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/fdio/directory.h>

#include <gtest/gtest.h>

#include "src/power/testing/system-integration/util/test_util.h"

class PowerSystemIntegration : public system_integration_utils::TestLoopBase, public testing::Test {
 public:
  void SetUp() override { Initialize(); }
};

TEST_F(PowerSystemIntegration, StorageSuspendResumeTest) {
  // To enable changing SAG's power levels, first trigger the "boot complete" logic. This is done by
  // setting both exec state level and app activity level to active.
  test_sagcontrol::SystemActivityGovernorState state = GetBootCompleteState();
  ASSERT_EQ(ChangeSagState(state), ZX_OK);

  auto result = component::Connect<fuchsia_diagnostics::ArchiveAccessor>();
  ASSERT_EQ(ZX_OK, result.status_value());
  diagnostics::reader::ArchiveReader reader(dispatcher(), {}, std::move(result.value()));

  const std::string sag_moniker = "bootstrap/system-activity-governor/system-activity-governor";
  const std::vector<std::string> sag_exec_state_level = {"root", "power_elements",
                                                         "execution_state", "power_level"};

  const std::string pb_moniker = "bootstrap/power-broker";
  const auto aml_sdmmc_element_id = GetPowerElementId(reader, pb_moniker, "aml-sdmmc-hardware");
  ASSERT_TRUE(aml_sdmmc_element_id.is_ok());
  const std::vector<std::string> aml_sdmmc_required_level = {
      "root",     "broker",
      "topology", "fuchsia.inspect.Graph",
      "topology", aml_sdmmc_element_id.value(),
      "meta",     "required_level"};
  const std::vector<std::string> aml_sdmmc_current_level = {
      "root",     "broker",
      "topology", "fuchsia.inspect.Graph",
      "topology", aml_sdmmc_element_id.value(),
      "meta",     "current_level"};
  const auto core_sdmmc_element_id = GetPowerElementId(reader, pb_moniker, "sdmmc-hardware");
  ASSERT_TRUE(core_sdmmc_element_id.is_ok());
  const std::vector<std::string> core_sdmmc_required_level = {
      "root",     "broker",
      "topology", "fuchsia.inspect.Graph",
      "topology", core_sdmmc_element_id.value(),
      "meta",     "required_level"};
  const std::vector<std::string> core_sdmmc_current_level = {
      "root",     "broker",
      "topology", "fuchsia.inspect.Graph",
      "topology", core_sdmmc_element_id.value(),
      "meta",     "current_level"};

  // Driver monikers are unstable, so wildcard the moniker and use a tree name
  const std::string aml_sdmmc_inspect_tree_name = "aml-sd-emmc";
  const std::string aml_sdmmc_moniker = "bootstrap/*-drivers*mmc-ffe07000*";
  // TODO(b/344044167): Fix inspect node names in aml-sdmmc driver.
  const std::vector<std::string> aml_sdmmc_suspended = {"root", "aml-sdmmc-port-unknown",
                                                        "power_suspended"};

  // Driver monikers are unstable, so wildcard the moniker and use a tree name
  const std::string core_sdmmc_moniker = "bootstrap/*-drivers*mmc-ffe07000*";
  const std::string sdmmc_root_device_inspect_tree_name = "sdmmc";
  const std::vector<std::string> core_sdmmc_suspended = {"root", "sdmmc_core", "power_suspended"};

  // Verify boot complete state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: aml-sdmmc's power element on.
  // - aml-sdmmc, core sdmmc: not suspended
  MatchInspectData(reader, sag_moniker, std::nullopt, sag_exec_state_level,
                   uint64_t{2});  // kActive
  MatchInspectData(reader, pb_moniker, std::nullopt, aml_sdmmc_required_level, uint64_t{1});   // On
  MatchInspectData(reader, pb_moniker, std::nullopt, aml_sdmmc_current_level, uint64_t{1});    // On
  MatchInspectData(reader, pb_moniker, std::nullopt, core_sdmmc_required_level, uint64_t{1});  // On
  MatchInspectData(reader, pb_moniker, std::nullopt, core_sdmmc_current_level, uint64_t{1});   // On
  MatchInspectData(reader, aml_sdmmc_moniker, aml_sdmmc_inspect_tree_name, aml_sdmmc_suspended,
                   false);
  MatchInspectData(reader, core_sdmmc_moniker, sdmmc_root_device_inspect_tree_name,
                   core_sdmmc_suspended, false);

  // Emulate system suspend.
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kInactive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kInactive);
  ASSERT_EQ(ChangeSagState(state), ZX_OK);
  ASSERT_EQ(AwaitSystemSuspend(), ZX_OK);

  // Verify suspend state using inspect data:
  // - SAG: exec state level inactive
  // - Power Broker: aml-sdmmc's power element off.
  // - aml-sdmmc, core sdmmc: suspended
  MatchInspectData(reader, sag_moniker, std::nullopt, sag_exec_state_level,
                   uint64_t{0});  // kInactive
  MatchInspectData(reader, pb_moniker, std::nullopt, aml_sdmmc_required_level, uint64_t{0});  // Off
  MatchInspectData(reader, pb_moniker, std::nullopt, aml_sdmmc_current_level, uint64_t{0});   // Off
  MatchInspectData(reader, pb_moniker, std::nullopt, core_sdmmc_required_level,
                   uint64_t{0});                                                              // Off
  MatchInspectData(reader, pb_moniker, std::nullopt, core_sdmmc_current_level, uint64_t{0});  // Off
  MatchInspectData(reader, aml_sdmmc_moniker, aml_sdmmc_inspect_tree_name, aml_sdmmc_suspended,
                   true);
  MatchInspectData(reader, core_sdmmc_moniker, sdmmc_root_device_inspect_tree_name,
                   core_sdmmc_suspended, true);

  // Emulate system resume.
  ASSERT_EQ(StartSystemResume(), ZX_OK);
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kActive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kActive);
  ASSERT_EQ(ChangeSagState(state), ZX_OK);

  // Verify resume state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: aml-sdmmc's power element on.
  // - aml-sdmmc, core sdmmc: not suspended
  MatchInspectData(reader, sag_moniker, std::nullopt, sag_exec_state_level,
                   uint64_t{2});  // kActive
  MatchInspectData(reader, pb_moniker, std::nullopt, aml_sdmmc_required_level, uint64_t{1});   // On
  MatchInspectData(reader, pb_moniker, std::nullopt, aml_sdmmc_current_level, uint64_t{1});    // On
  MatchInspectData(reader, pb_moniker, std::nullopt, core_sdmmc_required_level, uint64_t{1});  // On
  MatchInspectData(reader, pb_moniker, std::nullopt, core_sdmmc_current_level, uint64_t{1});   // On
  MatchInspectData(reader, aml_sdmmc_moniker, aml_sdmmc_inspect_tree_name, aml_sdmmc_suspended,
                   false);
  MatchInspectData(reader, core_sdmmc_moniker, sdmmc_root_device_inspect_tree_name,
                   core_sdmmc_suspended, false);
}
