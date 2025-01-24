// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test_util.h"

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>

#include <gtest/gtest.h>

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

void Connector::Receive(ReceiveRequest& request, ReceiveCompleter::Sync& completer) {
  zx_status_t status = fdio_service_connect(path_.c_str(), request.channel().release());
  if (status != ZX_OK) {
    completer.Close(status);
  }
}

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
    auto result = component::Connect<fuchsia_driver_development::Manager>();
    ASSERT_EQ(ZX_OK, result.status_value());
    driver_manager_client_end_ = std::move(result.value());
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
    auto status = fidl::WireCall(sag_control_state_client_end_)->SetBootComplete();
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

zx::eventpair TestLoopBase::PrepareDriver(std::string_view node_filter, std::string_view driver_url,
                                          bool expect_new_koid) {
  // Find the node running our target driver.
  std::cout << "Preparing driver '" << driver_url << "' for test..." << std::endl;
  std::optional<std::string> found = std::nullopt;
  uint64_t old_koid;
  while (!found) {
    auto node_vec = GetNodeInfo(node_filter);
    for (auto& node : node_vec) {
      if (node.bound_driver_url().has_value() && node.bound_driver_url().value() == driver_url) {
        std::cout << "driver found with moniker '" << node.moniker().value() << "'" << std::endl;
        found.emplace(node.moniker().value());
        old_koid = node.driver_host_koid().value();
        break;
      }
    }

    if (!found) {
      std::cout << "driver not found, retrying... " << std::endl;
      // Small loop delay.
      RunLoopWithTimeout(zx::msec(10));
    }
  }

  std::cout << "restarting driver with test dictionary..." << std::endl;
  // Setup the power dictionary and restart the node with this dictionary.
  auto dict_ref = CreateDictionaryForTest();
  auto result =
      fidl::Call(driver_manager_client_end_)->RestartWithDictionary({*found, std::move(dict_ref)});
  EXPECT_EQ(true, result.is_ok());

  if (!expect_new_koid) {
    // Let the restart make progress, otherwise our next loop could run early enough to see the old
    // instance of the target node since its not doing the koid comparison.
    RunLoopWithTimeout(zx::sec(1));
  }

  std::cout << "checking for the driver to be up again..." << std::endl;
  found = std::nullopt;

  // Wait until the node is available again, possibly under a new driver host.
  while (!found) {
    auto node_vec = GetNodeInfo(node_filter);
    for (auto& node : node_vec) {
      if (node.bound_driver_url().has_value() && node.bound_driver_url().value() == driver_url) {
        if (node.driver_host_koid().has_value()) {
          if (!expect_new_koid || old_koid != node.driver_host_koid().value()) {
            found.emplace(node.moniker().value());
            break;
          }
        }
      }
    }

    if (!found) {
      std::cout << "driver not found, retrying... " << std::endl;
      // Small loop delay.
      RunLoopWithTimeout(zx::msec(10));
    }
  }

  std::cout << "proceeding with test! " << std::endl;
  // Return the release_fence for the caller to hold on to.
  return std::move(result.value().release_fence());
}

fuchsia_component_sandbox::DictionaryRef TestLoopBase::CreateDictionaryForTest() {
  // Start a background loop to run the sandbox connectors.
  sandbox_connector_loop_.StartThread("sandbox-loop");

  // Counter for the capability store.
  auto cap_store = component::Connect<fuchsia_component_sandbox::CapabilityStore>();
  uint32_t dict_id = next_cap_id_++;
  auto capstore_result = fidl::Call(*cap_store)->DictionaryCreate({dict_id});
  EXPECT_EQ(true, capstore_result.is_ok());

  auto [sag_client, sag_server] = fidl::Endpoints<fuchsia_component_sandbox::Receiver>::Create();
  uint32_t sag_id = next_cap_id_++;
  auto connector_res = fidl::Call(*cap_store)->ConnectorCreate({sag_id, {std::move(sag_client)}});
  EXPECT_EQ(true, connector_res.is_ok());

  auto [broker_client, broker_server] =
      fidl::Endpoints<fuchsia_component_sandbox::Receiver>::Create();
  uint32_t broker_id = next_cap_id_++;
  connector_res = fidl::Call(*cap_store)->ConnectorCreate({broker_id, {std::move(broker_client)}});
  EXPECT_EQ(true, connector_res.is_ok());

  auto [cpu_element_client, cpu_element_server] =
      fidl::Endpoints<fuchsia_component_sandbox::Receiver>::Create();
  uint32_t cpu_element_id = next_cap_id_++;
  connector_res =
      fidl::Call(*cap_store)->ConnectorCreate({cpu_element_id, {std::move(cpu_element_client)}});
  EXPECT_EQ(true, connector_res.is_ok());

  auto insert_res =
      fidl::Call(*cap_store)
          ->DictionaryInsert({dict_id, {"fuchsia.power.system.ActivityGovernor", sag_id}});
  EXPECT_EQ(true, insert_res.is_ok());

  insert_res = fidl::Call(*cap_store)
                   ->DictionaryInsert({dict_id, {"fuchsia.power.broker.Topology", broker_id}});
  EXPECT_EQ(true, insert_res.is_ok());

  insert_res =
      fidl::Call(*cap_store)
          ->DictionaryInsert({dict_id, {"fuchsia.power.system.CpuElementManager", cpu_element_id}});
  EXPECT_EQ(true, insert_res.is_ok());

  auto dict_export = fidl::Call(*cap_store)->Export({dict_id});
  auto dict_ref = std::move(dict_export->capability().dictionary().value());

  sag_connector_.emplace(async_patterns::PassDispatcher,
                         std::string("/svc/fuchsia.power.system.ActivityGovernor"),
                         std::move(sag_server));

  broker_connector_.emplace(async_patterns::PassDispatcher,
                            std::string("/svc/fuchsia.power.broker.Topology"),
                            std::move(broker_server));

  cpu_element_connector_.emplace(async_patterns::PassDispatcher,
                                 std::string("/svc/fuchsia.power.system.CpuElementManager"),
                                 std::move(cpu_element_server));

  return dict_ref;
}

std::vector<fuchsia_driver_development::NodeInfo> TestLoopBase::GetNodeInfo(
    std::string_view node_filter) {
  std::vector<fuchsia_driver_development::NodeInfo> result;

  auto [info_client, info_server] =
      fidl::Endpoints<fuchsia_driver_development::NodeInfoIterator>::Create();

  auto info = fidl::Call(driver_manager_client_end_)
                  ->GetNodeInfo({{std::string(node_filter)}, std::move(info_server), false});
  EXPECT_EQ(true, info.is_ok());

  while (true) {
    auto info_next = fidl::Call(info_client)->GetNext();
    EXPECT_EQ(true, info_next.is_ok());

    if (info_next->nodes().empty()) {
      break;
    }

    for (auto& node : info_next->nodes()) {
      result.push_back(node);
    }
  }

  return result;
}

}  // namespace system_integration_utils
