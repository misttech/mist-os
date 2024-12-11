// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_POWER_TESTING_SYSTEM_INTEGRATION_UTIL_TEST_UTIL_H_
#define SRC_POWER_TESTING_SYSTEM_INTEGRATION_UTIL_TEST_UTIL_H_

#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/natural_ostream.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>

namespace system_integration_utils {

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

 private:
  fidl::ClientEnd<test_sagcontrol::State> sag_control_state_client_end_;
  fidl::ClientEnd<test_suspendcontrol::Device> suspend_device_client_end_;
};

}  // namespace system_integration_utils

#endif  // SRC_POWER_TESTING_SYSTEM_INTEGRATION_UTIL_TEST_UTIL_H_
