// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_PROFILER_CONTROLLER_IMPL_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_PROFILER_CONTROLLER_IMPL_H_

#include <fcntl.h>
#include <fidl/fuchsia.cpu.profiler/cpp/fidl.h>
#include <lib/zx/process.h>
#include <lib/zx/task.h>
#include <lib/zx/thread.h>
#include <zircon/compiler.h>
#include <zircon/system/ulib/elf-search/include/elf-search.h>

#include "component.h"
#include "sampler.h"
#include "src/lib/fxl/memory/ref_ptr.h"
#include "targets.h"

namespace profiler {
class ProfilerControllerImpl : public fidl::Server<fuchsia_cpu_profiler::Session> {
 public:
  ProfilerControllerImpl(async_dispatcher_t* dispatcher, ComponentWatcher& event_stream)
      : dispatcher_(dispatcher), component_watcher_(event_stream) {}
  void Configure(ConfigureRequest& request, ConfigureCompleter::Sync& completer) override;
  void Start(StartRequest& request, StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void Reset(ResetCompleter::Sync& completer) override;

  ~ProfilerControllerImpl() override = default;

 private:
  void Reset();
  zx::socket socket_;

  enum ProfilingState {
    Unconfigured,
    Running,
    Stopped,
  };
  async_dispatcher_t* dispatcher_;
  // LIFETIME: Reference is to a ComponentWatcher created in main() which lives for the life of the
  // program.
  ComponentWatcher& component_watcher_;
  fxl::RefPtr<Sampler> sampler_;
  ProfilingState state_ = ProfilingState::Unconfigured;

  TargetTree targets_;
  elf_search::Searcher searcher_;
  std::vector<fuchsia_cpu_profiler::SamplingConfig> sample_specs_;
  std::unique_ptr<ComponentTarget> component_target_;
};
}  // namespace profiler

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_PROFILER_CONTROLLER_IMPL_H_
