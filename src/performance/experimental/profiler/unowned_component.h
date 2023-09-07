// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_UNOWNED_COMPONENT_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_UNOWNED_COMPONENT_H_

#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.sys2/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/result.h>

#include "component.h"
#include "component_watcher.h"

namespace profiler {

// A component that is to be profiled, but who's lifecycle is not controlled by the profiler
class UnownedComponent : public Component {
 public:
  explicit UnownedComponent(async_dispatcher_t* dispatcher) : Component(dispatcher) {}
  static zx::result<std::unique_ptr<Component>> Create(async_dispatcher_t* dispatcher,
                                                       const std::string& moniker);
  zx::result<> Start(ComponentWatcher::ComponentEventHandler on_start = nullptr) override;
  zx::result<> Stop() override;
  zx::result<> Destroy() override;
};

}  // namespace profiler

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_UNOWNED_COMPONENT_H_
