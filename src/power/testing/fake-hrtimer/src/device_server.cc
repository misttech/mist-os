// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device_server.h"

#include <fidl/fuchsia.hardware.hrtimer/cpp/natural_types.h>
#include <lib/syslog/cpp/macros.h>

#include <limits>
#include <optional>
#include <vector>

namespace fake_hrtimer {

using fuchsia_hardware_hrtimer::DeviceGetTicksLeftResponse;
using fuchsia_hardware_hrtimer::Properties;
using fuchsia_hardware_hrtimer::Resolution;
using fuchsia_hardware_hrtimer::TimerProperties;

DeviceServer::DeviceServer() = default;

void DeviceServer::Start(StartRequest& _request, StartCompleter::Sync& completer) {
  completer.Reply(zx::ok());
}

void DeviceServer::Stop(StopRequest& _request, StopCompleter::Sync& completer) {
  completer.Reply(zx::ok());
  if (event_) {
    event_->signal(0, ZX_EVENT_SIGNALED);
  }
}

void DeviceServer::GetTicksLeft(GetTicksLeftRequest& _request,
                                GetTicksLeftCompleter::Sync& completer) {
  completer.Reply(zx::ok(DeviceGetTicksLeftResponse().ticks(0)));
}

void DeviceServer::SetEvent(SetEventRequest& request, SetEventCompleter::Sync& completer) {
  event_.emplace(std::move(request.event()));
  completer.Reply(zx::ok());
}

void DeviceServer::StartAndWait(StartAndWaitRequest& request,
                                StartAndWaitCompleter::Sync& completer) {
  completer.Reply(zx::error_result());
}

void DeviceServer::GetProperties(GetPropertiesCompleter::Sync& completer) {
  uint64_t size = 10;
  std::vector<TimerProperties> timer_properties(size);
  for (uint64_t i = 0; i < size; i++) {
    timer_properties[i] =
        TimerProperties()
            .id(i)
            .max_ticks(std::numeric_limits<uint16_t>::max())
            .supports_event(true)
            .supports_wait(true)
            .supported_resolutions(std::vector<Resolution>{{Resolution::WithDuration(1000000)}});
  }
  Properties properties = {};
  properties.timers_properties(std::move(timer_properties));
  completer.Reply(std::move(properties));
}

void DeviceServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_hrtimer::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Received an unknown method with ordinal " << metadata.method_ordinal;
}

void DeviceServer::Serve(async_dispatcher_t* dispatcher,
                         fidl::ServerEnd<fuchsia_hardware_hrtimer::Device> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_hrtimer
