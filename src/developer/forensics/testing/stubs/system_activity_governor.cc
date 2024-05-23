// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/system_activity_governor.h"

#include <lib/zx/event.h>

#include <utility>

#include "src/lib/testing/predicates/status.h"

namespace forensics::stubs {

void SystemActivityGovernor::GetPowerElements(GetPowerElementsCompleter::Sync& completer) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  fuchsia_power_system::ExecutionState execution_state;
  execution_state.passive_dependency_token(std::move(event));

  fuchsia_power_system::PowerElements elements;
  elements.execution_state(std::move(execution_state));

  completer.Reply(fidl::Response<fuchsia_power_system::ActivityGovernor::GetPowerElements>(
      std::move(elements)));
}

void SystemActivityGovernorNoTokens::GetPowerElements(GetPowerElementsCompleter::Sync& completer) {
  fuchsia_power_system::PowerElements elements;
  elements.execution_state(fuchsia_power_system::ExecutionState());

  completer.Reply(fidl::Response<fuchsia_power_system::ActivityGovernor::GetPowerElements>(
      std::move(elements)));
}

}  // namespace forensics::stubs
