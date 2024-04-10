// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_KERNEL_SAMPLER_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_KERNEL_SAMPLER_H_

#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls-next.h>

#include "sampler.h"

namespace profiler {

// Wrapper over zx_sampler_start
class KernelSamplerSession {
 public:
  explicit KernelSamplerSession(zx::iob per_cpu_buffers)
      : per_cpu_buffers_(std::move(per_cpu_buffers)) {}
  static zx::result<std::unique_ptr<KernelSamplerSession>> CreateAndInit(
      const zx_sampler_config_t& config);

  zx::result<> Start();
  zx::result<> Stop();
  zx::result<> AttachThread(const zx::thread& thread) const;
  zx::result<zx::iob> GetBuffers() {
    zx::iob duplicate;
    return zx::make_result(per_cpu_buffers_.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate),
                           std::move(duplicate));
  }

  ~KernelSamplerSession() = default;

 private:
  bool running_ = false;
  zx::iob per_cpu_buffers_;
};

class KernelSampler : public Sampler {
 public:
  explicit KernelSampler(async_dispatcher_t* dispatcher, TargetTree&& targets)
      : Sampler(dispatcher, std::move(targets)) {}

  zx::result<> AddTarget(JobTarget&& target) override;
  zx::result<> Start(size_t buffer_size_mb) override;
  zx::result<> Stop() override;

 private:
  void AddThread(std::vector<const zx_koid_t> job_path, zx_koid_t pid, zx_koid_t tid,
                 zx::thread t) override;
  void RemoveThread(std::vector<const zx_koid_t> job_path, zx_koid_t pid, zx_koid_t tid) override;

  std::unique_ptr<KernelSamplerSession> session_;
  size_t buffer_size_bytes_;
};
}  // namespace profiler
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_KERNEL_SAMPLER_H_
