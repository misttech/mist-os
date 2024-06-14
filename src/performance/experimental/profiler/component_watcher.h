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
  zx::result<> Reset();
  void HandleEvent(fidl::Result<fuchsia_component::EventStream::GetNext>& res);

  using ComponentEventHandler = fit::function<void(std::string moniker, std::string url)>;

  // Run a handler when we receive a start/stop event for a moniker
  zx::result<> WatchForMoniker(std::string moniker, ComponentEventHandler handler);

  // Run a handler when we receive a start/stop event for a url.
  //
  // This is less precise than watching for a moniker since multiple components may share a url, but
  // is used for when we don't handle launching the component directly and don't know the moniker,
  // such as with tests.
  zx::result<> WatchForUrl(std::string url, ComponentEventHandler handler);

 private:
  fidl::Client<fuchsia_component::EventStream> stream_client_;
  async_dispatcher_t* dispatcher_;

  std::map<std::string, ComponentEventHandler> moniker_watchers_;
  std::map<std::string, ComponentEventHandler> url_watchers_;
};
}  // namespace profiler

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_WATCHER_H_
