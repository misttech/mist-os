// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_POWER_TESTING_SYSTEM_INTEGRATION_UTIL_TEST_UTIL_H_
#define SRC_POWER_TESTING_SYSTEM_INTEGRATION_UTIL_TEST_UTIL_H_

#include <fidl/fuchsia.component.sandbox/cpp/fidl.h>
#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/natural_ostream.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>

namespace system_integration_utils {

class Connector final : public fidl::Server<fuchsia_component_sandbox::Receiver> {
 public:
  explicit Connector(async_dispatcher_t* dispatcher, std::string path,
                     fidl::ServerEnd<fuchsia_component_sandbox::Receiver> server)
      : path_(std::move(path)),
        binding_(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure) {}

  void Receive(ReceiveRequest& request, ReceiveCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_component_sandbox::Receiver> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  std::string path_;
  fidl::ServerBinding<fuchsia_component_sandbox::Receiver> binding_;
};

class TestLoopBase : public loop_fixture::RealLoop {
 protected:
  void Initialize();

  test_sagcontrol::SystemActivityGovernorState GetBootCompleteState();

  bool SetBootComplete();

  zx_status_t AwaitSystemSuspend();
  zx_status_t StartSystemResume();

  // Change the SAG state and wait for the transition to complete.
  zx_status_t ChangeSagState(test_sagcontrol::SystemActivityGovernorState state,
                             zx::duration poll_delay = zx::sec(1));

  // Wait for an inspect selector to match a specific value.
  void MatchInspectData(diagnostics::reader::ArchiveReader& reader, const std::string& moniker,
                        const std::optional<std::string>& inspect_tree_name,
                        const std::vector<std::string>& inspect_path,
                        std::variant<bool, uint64_t> value);

  zx::result<std::string> GetPowerElementId(diagnostics::reader::ArchiveReader& reader,
                                            const std::string& pb_moniker,
                                            const std::string& power_element_name);

  // Prepare the target driver for power system testing. This is done by creating a dictionary
  // with the power protocols from the test-specific instances of the SAG and power broker,
  // and restarting the target driver and its children with access to this dictionary.
  //
  // |expect_new_koid| whether to expect the restarted node to have a new driver host koid, this
  // should be true if the target driver is not colocated with its parent driver, otherwise it
  // should be false.
  //
  // Returns a zx::eventpair that should be held onto for the duration of the test. When released
  // the target driver and children are restarted again and lose access to the test-specific
  // power protocols.
  zx::eventpair PrepareDriver(std::string_view node_filter, std::string_view driver_url,
                              bool expect_new_koid);

  // Create and export a component framework dictionary that contains connectors for the various
  // power framework protocols, that are connected to the test-specific SAG and power broker that
  // is accessible in the incoming namespace of the test component. See 'meta/client.shard.cml'.
  fuchsia_component_sandbox::DictionaryRef CreateDictionaryForTest();

  // Query the driver framework for nodes that have a moniker matching the |node_filter|.
  std::vector<fuchsia_driver_development::NodeInfo> GetNodeInfo(std::string_view node_filter);

 private:
  async::Loop sandbox_connector_loop_{&kAsyncLoopConfigNeverAttachToThread};
  uint32_t next_cap_id_ = 1;
  fidl::ClientEnd<test_sagcontrol::State> sag_control_state_client_end_;
  fidl::ClientEnd<test_suspendcontrol::Device> suspend_device_client_end_;
  fidl::ClientEnd<fuchsia_driver_development::Manager> driver_manager_client_end_;

  async_patterns::DispatcherBound<Connector> sag_connector_{sandbox_connector_loop_.dispatcher()};
  async_patterns::DispatcherBound<Connector> broker_connector_{
      sandbox_connector_loop_.dispatcher()};
  async_patterns::DispatcherBound<Connector> cpu_element_connector_{
      sandbox_connector_loop_.dispatcher()};
};

}  // namespace system_integration_utils

#endif  // SRC_POWER_TESTING_SYSTEM_INTEGRATION_UTIL_TEST_UTIL_H_
