// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/power/cpp/suspend.h>
#include <lib/fidl/cpp/channel.h>

namespace fdf_power {
namespace fps = fuchsia_power_system;

Completer::~Completer() {
  ZX_ASSERT_MSG(callback_ == std::nullopt, "Completer was not called before going out of scope.");
}

void Completer::operator()() {
  ZX_ASSERT_MSG(callback_ != std::nullopt, "Cannot call Completer more than once.");
  auto callback = std::move(callback_.value());
  callback_.reset();
  callback();
}

namespace internal {
zx::result<fidl::ServerEnd<fps::SuspendBlocker>> RegisterSuspendHooks(fdf::Namespace& incoming) {
  zx::result sag = incoming.Connect<fps::ActivityGovernor>();
  if (sag.is_error()) {
    return sag.take_error();
  }

  auto [client_end, server_end] = fidl::Endpoints<fps::SuspendBlocker>::Create();
  auto result = fidl::Call(sag.value())
                    ->RegisterSuspendBlocker(fps::ActivityGovernorRegisterSuspendBlockerRequest{{
                        .suspend_blocker = std::move(client_end),
                        .name = "driver",
                    }});
  if (result.is_error()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  return zx::ok(std::move(server_end));
}
}  // namespace internal

}  // namespace fdf_power
