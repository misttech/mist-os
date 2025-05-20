// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fidl/cpp/wire/string_view.h>
#include <lib/fidl/cpp/wire_natural_conversions.h>
#include <zircon/availability.h>

#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace power_lib_test {
class SystemActivityGovernor
    : public fidl::testing::TestBase<fuchsia_power_system::ActivityGovernor> {
 public:
  SystemActivityGovernor(zx::event exec_state_opportunistic, zx::event wake_handling_assertive,
                         async_dispatcher_t* dispatcher)
      : listener_(std::nullopt),
        exec_state_opportunistic_(std::move(exec_state_opportunistic)),
        wake_handling_assertive_(std::move(wake_handling_assertive)),
        dispatcher_(dispatcher) {}

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    fuchsia_power_system::PowerElements elements;
    zx::event execution_element;
    exec_state_opportunistic_.duplicate(ZX_RIGHT_SAME_RIGHTS, &execution_element);

    fuchsia_power_system::ExecutionState exec_state = {
        {.opportunistic_dependency_token = std::move(execution_element)}};

    elements = {{.execution_state = std::move(exec_state)}};

    completer.Reply({{std::move(elements)}});
  }

  void RegisterListener(RegisterListenerRequest& request,
                        RegisterListenerCompleter::Sync& completer) override {
    fidl::ClientEnd<fuchsia_power_system::ActivityGovernorListener> listener =
        std::move(request.listener().value());

    // only support one listener
    ASSERT_EQ(listener_, std::nullopt);

    listener_.emplace(fidl::Client<fuchsia_power_system::ActivityGovernorListener>(
        std::move(listener), dispatcher_));
    completer.Reply();
  }

  void SendSuspend() {
    if (!listener_) {
      return;
    }
    // If there's an error, do something?
    listener_.value()->OnSuspendStarted().ThenExactlyOnce(
        [](fidl::Result<fuchsia_power_system::ActivityGovernorListener::OnSuspendStarted>& result) {
        });
  }

  void SendResume() {
    if (!listener_) {
      return;
    }
    // If there's an error, do something?
    listener_.value()->OnResume().ThenExactlyOnce(
        [](fidl::Result<fuchsia_power_system::ActivityGovernorListener::OnResume>& result) {});
  }

  void AcquireWakeLease(AcquireWakeLeaseRequest& request,
                        AcquireWakeLeaseCompleter::Sync& completer) override {
    zx::eventpair one;
    zx::eventpair two;
    zx::eventpair::create(0, &one, &two);
    completer.Reply(fit::success<fuchsia_power_system::ActivityGovernorAcquireWakeLeaseResponse>(
        std::move(two)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE() << name << " is not implemented";
  }

 private:
  std::optional<fidl::Client<fuchsia_power_system::ActivityGovernorListener>> listener_;
  zx::event exec_state_opportunistic_;
  zx::event wake_handling_assertive_;
  async_dispatcher_t* dispatcher_;
};
}  // namespace power_lib_test

#endif
