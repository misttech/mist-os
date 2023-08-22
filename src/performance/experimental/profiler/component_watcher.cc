// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "component_watcher.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

zx::result<> profiler::ComponentWatcher::Watch() {
  auto client_end = component::Connect<fuchsia_component::EventStream>();
  if (client_end.is_error()) {
    FX_LOGS(ERROR) << "Failed to connect to event stream";
    return client_end.take_error();
  }
  stream_client_ = fidl::Client(std::move(*client_end), dispatcher_);

  stream_client_->WaitForReady().Then(
      [this](fidl::Result<fuchsia_component::EventStream::WaitForReady> &res) {
        if (res.is_error()) {
          FX_LOGS(ERROR) << "Failed to wait for EventStream to start";
          return;
        }
        stream_client_->GetNext().Then(
            [this](fidl::Result<fuchsia_component::EventStream::GetNext> &res) {
              this->HandleEvent(res);
            });
      });
  return zx::ok();
}

zx::result<> profiler::ComponentWatcher::WatchForMoniker(std::string moniker,
                                                         ComponentEventHandler handler) {
  // The events api strips leading "./"s from monikers
  std::string normalized = moniker;
  if (normalized[0] == '.' && normalized[1] == '/') {
    normalized = normalized.substr(2);
  }

  if (moniker_watchers_.find(normalized) != moniker_watchers_.end()) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }
  moniker_watchers_[std::move(normalized)] = std::move(handler);
  return zx::ok();
}

void profiler::ComponentWatcher::HandleEvent(
    fidl::Result<fuchsia_component::EventStream::GetNext> &res) {
  if (res.is_error()) {
    FX_LOGS(ERROR) << "GetEventFailed";
    return;
  }
  std::vector<fuchsia_component::Event> events = std::move(res->events());
  if (events.empty()) {
    return;
  }

  for (fuchsia_component::Event &event : events) {
    if (!event.header().has_value()) {
      continue;
    }
    if (!event.header()->moniker() || !event.header()->component_url()) {
      FX_LOGS(WARNING) << "Event didn't have a moniker or url?";
      continue;
    }
    std::string component_url = *event.header()->component_url();
    std::string moniker = *event.header()->moniker();
    if (event.header()->event_type()) {
      fuchsia_component::EventType event_type = event.header()->event_type().value();
      switch (event_type) {
        case fuchsia_component::EventType::kDebugStarted: {
          fidl::SyncClient<fuchsia_io::Directory> dir_client{
              std::move(*event.payload()->debug_started()->runtime_dir())};
          auto moniker_handler = moniker_watchers_.find(moniker);
          if (moniker_handler != moniker_watchers_.end()) {
            moniker_handler->second(moniker, component_url);
            moniker_watchers_.erase(moniker_handler);
          }
          break;
        }
        default:
          // We only have subscribed to debug started events
          break;
      }
    }
  }

  stream_client_->GetNext().Then(
      [this](fidl::Result<fuchsia_component::EventStream::GetNext> &res) {
        this->HandleEvent(res);
      });
}
