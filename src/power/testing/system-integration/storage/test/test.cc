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
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

template <typename T>
std::string ToString(const T& value) {
  std::ostringstream buf;
  buf << value;
  return buf.str();
}
template <typename T>
std::string FidlString(const T& value) {
  return ToString(fidl::ostream::Formatted<T>(value));
}

class PowerSystemIntegration : public gtest::RealLoopFixture {
 public:
  void SetUp() override {
    auto result = component::Connect<test_sagcontrol::State>();
    ASSERT_EQ(ZX_OK, result.status_value());
    sag_control_state_client_end_ = std::move(result.value());
  }

  zx_status_t ChangeSagState(test_sagcontrol::SystemActivityGovernorState state) {
    auto set_result = fidl::Call(sag_control_state_client_end_)->Set(state);
    if (!set_result.is_ok()) {
      std::cout << "Failed to set SAG state: " << set_result.error_value() << std::endl;
      return ZX_ERR_INTERNAL;
    }

    while (true) {
      const auto get_result = fidl::Call(sag_control_state_client_end_)->Get();
      if (get_result.value() == state) {
        break;
      }
      std::cout << "Waiting for the SAG state to change. Last known state: "
                << FidlString(get_result.value()) << std::endl;
      zx::nanosleep(zx::deadline_after(zx::sec(1)));
    }
    return ZX_OK;
  }

  void MatchInspectData(diagnostics::reader::ArchiveReader& reader, const std::string& moniker,
                        const std::vector<std::string>& inspect_path,
                        std::variant<bool, uint64_t> value) {
    bool match = false;
    do {
      auto result = RunPromise(reader.SnapshotInspectUntilPresent({moniker}));
      auto data = result.take_value();
      for (const auto& datum : data) {
        if (datum.moniker() == moniker) {
          bool* bool_value = std::get_if<bool>(&value);
          if (bool_value != nullptr && datum.GetByPath(inspect_path).GetBool() == *bool_value) {
            match = true;
            break;
          }

          uint64_t* uint64_value = std::get_if<uint64_t>(&value);
          if (uint64_value != nullptr &&
              datum.GetByPath(inspect_path).GetUint64() == *uint64_value) {
            match = true;
            break;
          }
        }
      }
    } while (!match);
  }

  zx::result<std::string> GetPowerElementId(diagnostics::reader::ArchiveReader& reader,
                                            const std::string& pb_moniker,
                                            const std::string& power_element_name) {
    auto result = RunPromise(reader.SnapshotInspectUntilPresent({pb_moniker}));
    auto data = result.take_value();
    for (const auto& datum : data) {
      if (datum.moniker() == pb_moniker && datum.payload().has_value()) {
        auto topology =
            datum.payload().value()->GetByPath({"topology", "fuchsia.inspect.Graph", "topology"});
        if (topology == nullptr) {
          return zx::error(ZX_ERR_BAD_STATE);
        }
        for (const auto& child : topology->children()) {
          auto name = datum
                          .GetByPath({"root", "topology", "fuchsia.inspect.Graph", "topology",
                                      child.name(), "meta", "name"})
                          .GetString();
          if (name == power_element_name) {
            return zx::ok(child.name());
          }
        }
      }
    }
    return zx::error(ZX_ERR_NOT_FOUND);
  }

 private:
  fidl::ClientEnd<test_sagcontrol::State> sag_control_state_client_end_;
};

TEST_F(PowerSystemIntegration, AmlSdmmcSuspendResumeTest) {
  // To enable changing SAG's power levels, first trigger the "boot complete" logic. This is done by
  // setting both exec state level and app activity level to active.
  test_sagcontrol::SystemActivityGovernorState state;
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kActive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kActive);
  state.full_wake_handling_level(fuchsia_power_system::FullWakeHandlingLevel::kInactive);
  state.wake_handling_level(fuchsia_power_system::WakeHandlingLevel::kInactive);
  ASSERT_EQ(ChangeSagState(state), ZX_OK);

  auto result = component::Connect<fuchsia_diagnostics::ArchiveAccessor>();
  ASSERT_EQ(ZX_OK, result.status_value());
  diagnostics::reader::ArchiveReader reader(dispatcher(), {}, std::move(result.value()));

  const std::string sag_moniker = "bootstrap/system-activity-governor/system-activity-governor";
  const std::vector<std::string> sag_inspect_path = {"root", "power_elements", "execution_state",
                                                     "power_level"};

  const std::string pb_moniker = "bootstrap/power-broker";
  const auto pb_aml_sdmmc_element_id = GetPowerElementId(reader, pb_moniker, "aml-sdmmc-hardware");
  ASSERT_TRUE(pb_aml_sdmmc_element_id.is_ok());
  const std::vector<std::string> pb_inspect_path_required = {"root", "required-levels",
                                                             pb_aml_sdmmc_element_id.value()};
  const std::vector<std::string> pb_inspect_path_current = {"root", "current-levels",
                                                            pb_aml_sdmmc_element_id.value()};

  const std::string aml_sdmmc_moniker = "bootstrap/boot-drivers:dev.sys.platform.05_00_8.aml_emmc";
  const std::vector<std::string> aml_sdmmc_inspect_path = {"root", "aml-sdmmc-portC",
                                                           "power_suspended"};

  // Verify boot complete state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: aml-sdmmc's power element on.
  // - aml-sdmmc: not suspended
  MatchInspectData(reader, sag_moniker, sag_inspect_path, uint64_t{2});         // kActive
  MatchInspectData(reader, pb_moniker, pb_inspect_path_required, uint64_t{1});  // On
  MatchInspectData(reader, pb_moniker, pb_inspect_path_current, uint64_t{1});   // On
  MatchInspectData(reader, aml_sdmmc_moniker, aml_sdmmc_inspect_path, false);

  // Emulate system suspend.
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kInactive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kInactive);
  ASSERT_EQ(ChangeSagState(state), ZX_OK);

  // Verify suspend state using inspect data:
  // - SAG: exec state level inactive
  // - Power Broker: aml-sdmmc's power element off.
  // - aml-sdmmc: suspended
  MatchInspectData(reader, sag_moniker, sag_inspect_path, uint64_t{0});         // kInactive
  MatchInspectData(reader, pb_moniker, pb_inspect_path_required, uint64_t{0});  // Off
  MatchInspectData(reader, pb_moniker, pb_inspect_path_current, uint64_t{0});   // Off
  MatchInspectData(reader, aml_sdmmc_moniker, aml_sdmmc_inspect_path, true);

  // Emulate system resume.
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kActive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kActive);
  ASSERT_EQ(ChangeSagState(state), ZX_OK);

  // Verify resume state using inspect data:
  // - SAG: exec state level active
  // - Power Broker: aml-sdmmc's power element on.
  // - aml-sdmmc: not suspended
  MatchInspectData(reader, sag_moniker, sag_inspect_path, uint64_t{2});         // kActive
  MatchInspectData(reader, pb_moniker, pb_inspect_path_required, uint64_t{1});  // On
  MatchInspectData(reader, pb_moniker, pb_inspect_path_current, uint64_t{1});   // On
  MatchInspectData(reader, aml_sdmmc_moniker, aml_sdmmc_inspect_path, false);
}
