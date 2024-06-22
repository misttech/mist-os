// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/assert.h>

int main(int argc, const char* argv[]) {
  FX_LOGS(INFO) << "Starting test controller...";
  ZX_ASSERT_MSG(argc > 1, "Got argc = %d, want > 1", argc);

  const bool should_succeed_suspend = strcmp(argv[1], "success") == 0;

  zx::result client_end = component::Connect<test_suspendcontrol::Device>();
  ZX_ASSERT_MSG(client_end.is_ok(),
                "Synchronous error when connecting to the |Device| protocol: %s",
                client_end.status_string());

  fidl::ClientEnd<test_suspendcontrol::Device> client = std::move(client_end.value());

  auto set_result = fidl::Call(client)->SetSuspendStates({{{{
      fuchsia_hardware_suspend::SuspendState{{.resume_latency = 100}},
  }}}});
  ZX_ASSERT(set_result.is_ok());

  while (true) {
    auto await_result = fidl::Call(client)->AwaitSuspend();
    ZX_ASSERT(await_result.is_ok());

    FX_LOGS(INFO) << "fake-suspend device attempted suspend";

    test_suspendcontrol::DeviceResumeRequest request =
        test_suspendcontrol::DeviceResumeRequest::WithError(ZX_OK);
    if (should_succeed_suspend) {
      fuchsia_hardware_suspend::WakeReason wake_reason;
      wake_reason.wake_vectors(std::vector<uint64_t>());
      wake_reason.soft_wake_vectors(std::vector<uint64_t>());

      test_suspendcontrol::SuspendResult suspend_result;
      suspend_result.reason(std::move(wake_reason));
      suspend_result.suspend_duration(0);
      suspend_result.suspend_overhead(0);

      request.result(std::move(suspend_result));
    } else {
      request.error(ZX_ERR_BAD_STATE);
    }

    auto resume_result = fidl::Call(client)->Resume(std::move(request));
    ZX_ASSERT(resume_result.is_ok());
  }

  return 0;
}
