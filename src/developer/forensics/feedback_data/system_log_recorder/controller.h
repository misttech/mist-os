// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_FEEDBACK_DATA_SYSTEM_LOG_RECORDER_CONTROLLER_H_
#define SRC_DEVELOPER_FORENSICS_FEEDBACK_DATA_SYSTEM_LOG_RECORDER_CONTROLLER_H_

#include <fuchsia/feedback/cpp/fidl.h>
#include <fuchsia/process/lifecycle/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>

namespace forensics {
namespace feedback_data {
namespace system_log_recorder {

class Controller : public fuchsia::process::lifecycle::Lifecycle {
 public:
  void SetStop(::fit::closure stop);

  // Immediately flushes the cached logs to disk.
  //
  // |fuchsia.process.lifecycle.Lifecycle|
  void Stop() override;

 private:
  ::fit::closure stop_;
};

}  // namespace system_log_recorder
}  // namespace feedback_data
}  // namespace forensics

#endif  // SRC_DEVELOPER_FORENSICS_FEEDBACK_DATA_SYSTEM_LOG_RECORDER_CONTROLLER_H_
