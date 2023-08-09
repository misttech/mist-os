// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_MEMORY_METRICS_FILTERS_H_
#define SRC_DEVELOPER_MEMORY_METRICS_FILTERS_H_

#include "src/developer/memory/metrics/capture.h"

namespace memory {
// When a job contains a process named |process_name|, filter out all other processes from that
// same job.
class FilterJobWithProcess : public memory::CaptureFilter {
 public:
  explicit FilterJobWithProcess(std::string process_name);

  void OnNewProcess(zx_koid_t job, zx_koid_t process, std::string_view name) override;

  bool ShouldCapture(zx_koid_t job, zx_koid_t process) override;

 private:
  const std::string process_name_;
  std::unordered_map<zx_koid_t, zx_koid_t> jobs_to_process_to_keep_;
};

}  // namespace memory

#endif  // SRC_DEVELOPER_MEMORY_METRICS_FILTERS_H_
