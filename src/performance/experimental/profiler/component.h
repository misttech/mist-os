// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_H_

#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.sys2/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/result.h>

namespace profiler {
class Component {
 public:
  explicit Component(async_dispatcher_t* dispatcher) {}
  // If on_start is specified, the callback will be called when it is successfully started via
  // `StartComponents`
  static zx::result<std::unique_ptr<Component>> Create(async_dispatcher_t* dispatcher,
                                                       const std::string& url,
                                                       const std::string& moniker);

  zx::result<> Start();
  zx::result<> Stop();
  zx::result<> Destroy();

  // Return the moniker the component was created at
  std::string Moniker() { return moniker_; }

  ~Component();

 private:
  fidl::SyncClient<fuchsia_sys2::LifecycleController> lifecycle_controller_client_;

  std::string name_;
  std::string collection_;
  std::string parent_moniker_;
  std::string moniker_;
  bool destroyed_ = false;
};
}  // namespace profiler

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_COMPONENT_H_
