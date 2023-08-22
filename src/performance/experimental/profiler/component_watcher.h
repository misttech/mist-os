// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_WATCHER_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_WATCHER_H_

#include <fidl/fuchsia.component/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/result.h>

namespace profiler {

class ComponentWatcher {
 public:
  explicit ComponentWatcher(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  zx::result<> Watch();
  void HandleEvent(fidl::Result<fuchsia_component::EventStream::GetNext>& res);

  using ComponentEventHandler = fit::function<void(std::string moniker, std::string url)>;
  zx::result<> WatchForMoniker(std::string moniker, ComponentEventHandler handler);

 private:
  fidl::Client<fuchsia_component::EventStream> stream_client_;
  async_dispatcher_t* dispatcher_;

  std::map<std::string, ComponentEventHandler> moniker_watchers_;
};
}  // namespace profiler

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_WATCHER_H_
