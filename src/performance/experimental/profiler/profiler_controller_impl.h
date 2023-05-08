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

#include "sampler.h"

class ProfilerControllerImpl : public fidl::Server<fuchsia_cpu_profiler::Session> {
 public:
  void Configure(ConfigureRequest& request, ConfigureCompleter::Sync& completer) override;
  void Start(StartRequest& request, StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void Reset(ResetCompleter::Sync& completer) override;

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fuchsia_cpu_profiler::Session> server_end);

  ~ProfilerControllerImpl() override = default;

 private:
  void Reset() __TA_REQUIRES(state_lock_);
  zx::socket socket_ __TA_GUARDED(state_lock_);

  enum ProfilingState {
    Unconfigured,
    Running,
    Stopped,
  };
  std::mutex state_lock_;
  std::unique_ptr<Sampler> sampler_ __TA_GUARDED(state_lock_);
  ProfilingState state_ __TA_GUARDED(state_lock_) = ProfilingState::Unconfigured;

  std::vector<SamplingInfo> targets_;
};

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_PROFILER_CONTROLLER_IMPL_H_
