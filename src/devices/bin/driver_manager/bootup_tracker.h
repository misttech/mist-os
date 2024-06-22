// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_BOOTUP_TRACKER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_BOOTUP_TRACKER_H_

#include <lib/async/cpp/task.h>

#include "src/devices/bin/driver_manager/bind/bind_manager.h"

namespace driver_manager {

class BootupTracker {
 public:
  BootupTracker(BindManager* manager, async_dispatcher_t* dispatcher)
      : bind_manager_(manager), dispatcher_(dispatcher) {}

  virtual ~BootupTracker() {}

  void WaitForBootup(fit::callback<void()> callback);

  // Starts the bootup tracker timeout task.
  void Start();

  // Called when there's a new request to start a driver for the given node.
  void NotifyNewStartRequest(std::string node_moniker, std::string driver_url);

  // Called when a start driver request is completed for the given node.
  void NotifyStartComplete(std::string node_moniker);

  // Called when the ongoing bind state in the bind manager has changed.
  void NotifyBindingChanged();

 protected:
  // Exposed for testing.
  virtual void ResetBootupTimer();

  // Exposed for testing.
  virtual bool IsUpdateDeadlineExceeded() const;

  // Invoked by |bootup_timeout_task_|. Exposed for testing.
  void OnBootupTimeout();

 private:
  void CheckBootupDone();
  void UpdateTrackerAndResetTimer();

  // Contains all outstanding start requests. Maps the node's component moniker to a driver url.
  std::unordered_map<std::string, std::string> outstanding_start_requests_;

  BindManager* bind_manager_;

  bool bootup_done_ = false;
  bool bootup_timeout_ = false;

  async_dispatcher_t* const dispatcher_;

  // Timestamp on when the bootup tracker was last updated. Used to check if a deadline for tracker
  // updates has been exceeded. If the deadline is exceeded, the bootup tracker completes bootup.
  zx::time last_update_timestamp_;

  // Stored WaitForBootup callbacks. Invoked once bootup is completed.
  std::vector<fit::callback<void()>> callbacks_;

  // Recurring task to check if bootup is complete.
  async::TaskClosureMethod<BootupTracker, &BootupTracker::OnBootupTimeout> bootup_timeout_task_{
      this};
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_BOOTUP_TRACKER_H_
