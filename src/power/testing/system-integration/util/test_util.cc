// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test_util.h"

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <gtest/gtest.h>

#include "fidl/fuchsia.hardware.suspend/cpp/natural_types.h"
#include "fidl/fuchsia.power.system/cpp/natural_types.h"
#include "fidl/test.suspendcontrol/cpp/natural_types.h"

namespace system_integration_utils {
namespace {

namespace fh_suspend = fuchsia_hardware_suspend;

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

}  // namespace

void TestLoopBase::Initialize() {
  {
    auto result = component::Connect<test_sagcontrol::State>();
    ASSERT_EQ(ZX_OK, result.status_value());
    sag_control_state_client_end_ = std::move(result.value());
  }

  {
    auto result = component::Connect<test_suspendcontrol::Device>();
    ASSERT_EQ(ZX_OK, result.status_value());
    suspend_device_client_end_ = std::move(result.value());
  }

  {
    fh_suspend::SuspendState state;
    state.resume_latency(zx::usec(100).to_nsecs());

    test_suspendcontrol::DeviceSetSuspendStatesRequest request;
    std::vector<fh_suspend::SuspendState> states = {state};
    request.suspend_states(states);

    auto result = fidl::Call(suspend_device_client_end_)->SetSuspendStates(request);
    ASSERT_TRUE(result.is_ok());
  }
}

test_sagcontrol::SystemActivityGovernorState TestLoopBase::GetBootCompleteState() {
  test_sagcontrol::SystemActivityGovernorState state;
  state.execution_state_level(fuchsia_power_system::ExecutionStateLevel::kActive);
  state.application_activity_level(fuchsia_power_system::ApplicationActivityLevel::kActive);
  return state;
}

zx_status_t TestLoopBase::AwaitSystemSuspend() {
  std::cout << "Awaiting suspend" << std::endl;
  auto wait_result = fidl::Call(suspend_device_client_end_)->AwaitSuspend();
  if (!wait_result.is_ok()) {
    std::cout << "Failed to await suspend: " << wait_result.error_value() << std::endl;
    return ZX_ERR_INTERNAL;
  }
  std::cout << "Suspend confirmed" << std::endl;

  return ZX_OK;
}

bool TestLoopBase::SetBootComplete() {
  {
    auto result = component::Connect<fuchsia_power_system::BootControl>();
    if (result.status_value()) {
      std::cout << "Failed to connect to BootControl" << std::endl;
      return false;
    }
    auto client_end = std::move(result.value());
    auto status = fidl::WireCall(client_end)->SetBootComplete();
    if (!status.ok()) {
      std::cout << "Failed to SetBootComplete" << std::endl;
      return false;
    }
  }
  return true;
}

zx_status_t TestLoopBase::StartSystemResume() {
  std::cout << "Starting system resume" << std::endl;
  test_suspendcontrol::SuspendResult suspend_result;
  // Assign suspend results to provide the appearance of a normal return from suspension.
  suspend_result.suspend_duration(2);
  suspend_result.suspend_overhead(1);
  auto request = test_suspendcontrol::DeviceResumeRequest::WithResult(suspend_result);
  auto resume_result = fidl::Call(suspend_device_client_end_)->Resume(request);
  if (!resume_result.is_ok()) {
    std::cout << "Failed to await suspend: " << resume_result.error_value() << std::endl;
    return ZX_ERR_INTERNAL;
  }
  std::cout << "Resume started" << std::endl;

  return ZX_OK;
}

zx_status_t TestLoopBase::ChangeSagState(test_sagcontrol::SystemActivityGovernorState state,
                                         zx::duration poll_delay) {
  while (true) {
    std::cout << "Setting SAG state: " << FidlString(state) << std::endl;
    auto set_result = fidl::Call(sag_control_state_client_end_)->Set(state);
    if (!set_result.is_ok()) {
      std::cout << "Failed to set SAG state: " << set_result.error_value() << std::endl;
      return ZX_ERR_INTERNAL;
    }
    zx::nanosleep(zx::deadline_after(poll_delay));

    const auto get_result = fidl::Call(sag_control_state_client_end_)->Get();
    if (get_result.value() == state) {
      break;
    }

    std::cout << "Retrying SAG state change. Last known state: " << FidlString(get_result.value())
              << std::endl;
    set_result = fidl::Call(sag_control_state_client_end_)->Set(GetBootCompleteState());
    if (!set_result.is_ok()) {
      std::cout << "Failed to set boot complete SAG state: " << set_result.error_value()
                << std::endl;
      return ZX_ERR_INTERNAL;
    }
    zx::nanosleep(zx::deadline_after(poll_delay));
  }
  std::cout << "SAG state change complete." << std::endl;
  return ZX_OK;
}

void TestLoopBase::MatchInspectData(diagnostics::reader::ArchiveReader& reader,
                                    const std::string& moniker,
                                    const std::optional<std::string>& inspect_tree_name,
                                    const std::vector<std::string>& inspect_path,
                                    std::variant<bool, uint64_t> value) {
  const auto selector = diagnostics::reader::MakeSelector(
      moniker,
      inspect_tree_name.has_value()
          ? std::optional<std::vector<std::string>>{{inspect_tree_name.value()}}
          : std::nullopt,
      std::vector<std::string>(inspect_path.begin(), inspect_path.end() - 1),
      inspect_path[inspect_path.size() - 1]);

  std::cout << "Matching inspect data for moniker = " << moniker << ", path = ";
  for (const auto& path : inspect_path) {
    std::cout << "[" << path << "]";
  }
  std::cout << ", selector = " << selector << std::endl;

  bool match = false;
  do {
    auto result = RunPromise(reader.SetSelectors({selector}).GetInspectSnapshot());
    auto data = result.take_value();
    for (const auto& datum : data) {
      bool* bool_value = std::get_if<bool>(&value);
      if (bool_value != nullptr) {
        const auto& actual_value = datum.GetByPath(inspect_path);
        // IsBool will return false if the value at inspect_path doesn't exist yet,
        // whereas GetBool will crash the test
        if (!actual_value.IsBool()) {
          std::cout << std::boolalpha << moniker << ": Expected value " << *bool_value
                    << ", but got nothing for selector " << selector << ". Taking another snapshot."
                    << std::endl;
          // look through all the trees
          continue;
        }
        if (actual_value.GetBool() == *bool_value) {
          match = true;
          std::cout << std::boolalpha << moniker << ": Got expected value " << *bool_value
                    << " for selector " << selector << std::endl;
          break;
        } else {
          std::cout << std::boolalpha << moniker << ": Expected value " << *bool_value
                    << ", but got " << actual_value.GetBool() << ". Taking another snapshot."
                    << std::endl;
        }
      }

      uint64_t* uint64_value = std::get_if<uint64_t>(&value);
      if (uint64_value != nullptr) {
        const auto& actual_value = datum.GetByPath(inspect_path);
        // IsUint64 will return false if the value at inspect_path doesn't exist yet,
        // whereas GetUint64 will crash the test
        if (!actual_value.IsUint64()) {
          std::cout << moniker << ": Expected value " << *uint64_value
                    << ", but got nothing for selector " << selector << ". Taking another snapshot."
                    << std::endl;
          // look through all the trees
          continue;
        }
        if (actual_value.GetUint64() == *uint64_value) {
          match = true;
          std::cout << moniker << ": Got expected value " << *uint64_value << " for selector "
                    << selector << std::endl;
          break;
        } else {
          std::cout << moniker << ": Expected value " << *uint64_value << ", but got "
                    << actual_value.GetUint64() << ". Taking another snapshot." << std::endl;
        }
      }
    }
  } while (!match);
}

zx::result<std::string> TestLoopBase::GetPowerElementId(diagnostics::reader::ArchiveReader& reader,
                                                        const std::string& pb_moniker,
                                                        const std::string& power_element_name) {
  std::cout << "Searching for power element with name '" << power_element_name
            << "' in Power Broker's topology listing." << std::endl;
  while (true) {
    auto result = RunPromise(reader.SnapshotInspectUntilPresent({pb_moniker}));
    auto data = result.take_value();
    for (const auto& datum : data) {
      if (datum.moniker() == pb_moniker && datum.payload().has_value()) {
        auto topology = datum.payload().value()->GetByPath(
            {"broker", "topology", "fuchsia.inspect.Graph", "topology"});
        if (topology == nullptr) {
          std::cout << "No topology listing in Power Broker's inspect data. ";
          break;
        }
        for (const auto& child : topology->children()) {
          auto name = datum
                          .GetByPath({"root", "broker", "topology", "fuchsia.inspect.Graph",
                                      "topology", child.name(), "meta", "name"})
                          .GetString();
          if (name == power_element_name) {
            std::cout << "Power element '" << power_element_name << "' has ID '" << child.name()
                      << "'." << std::endl;
            return zx::ok(child.name());
          }
        }
        std::cout << "Did not find power element in Power Broker's topology listing. ";
      }
    }
    std::cout << "Taking another snapshot." << std::endl;
  }
}

}  // namespace system_integration_utils
