// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/filters.h"

namespace memory {
FilterJobWithProcess::FilterJobWithProcess(std::string process_name)
    : process_name_(std::move(process_name)) {}

void FilterJobWithProcess::OnNewProcess(zx_koid_t job, zx_koid_t process, std::string_view name) {
  if (process_name_ == name) {
    jobs_to_process_to_keep_[job] = process;
  }
}

bool FilterJobWithProcess::ShouldCapture(zx_koid_t job, zx_koid_t process) {
  auto starnix_proc = jobs_to_process_to_keep_.find(job);
  return starnix_proc == jobs_to_process_to_keep_.end() || starnix_proc->second == process;
}

}  // namespace memory
