// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_BIN_SYSTEM_MONITOR_SYSTEM_MONITOR_H_
#define SRC_UI_BIN_SYSTEM_MONITOR_SYSTEM_MONITOR_H_

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>

namespace system_monitor {

// This class uses archiveAccessor and batchIterator to get diagnostic
// information like platform metrics which include CPU usage.
class SystemMonitor {
 public:
  // Class constructor initializes the batchIterator pointer to be used
  //  by the function to get all the inspect diagnostic.
  SystemMonitor();
  // This functions uses the iterator to access inspect diagnostic and turn
  // all the json format content into std::string and
  // returns a vector of strings.
  std::vector<std::string> updateRecentDiagnostic();

 private:
  // a vector of strings initialized and returned by updateRecentDiagnostic()
  std::vector<std::string> recentDiagnostic;
  // a batchIterator pointer initialized in the class constructor.
  fuchsia::diagnostics::BatchIteratorSyncPtr iterator;
};
}  // namespace system_monitor

#endif  // SRC_UI_BIN_SYSTEM_MONITOR_SYSTEM_MONITOR_H_
