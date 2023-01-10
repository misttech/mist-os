// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_RUNTIME_TESTING_RUNTIME_DISPATCHER_H_
#define LIB_DRIVER_RUNTIME_TESTING_RUNTIME_DISPATCHER_H_

#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/testing.h>
#include <lib/sync/cpp/completion.h>

namespace fdf {

// Run `task` on `dispatcher`, and block until it is completed.
// Because this blocks, this cannot be run on the dispatcher's thread.
void RunOnDispatcherSync(async_dispatcher_t* dispatcher, fit::closure task);

// A wrapper around an fdf::SynchronizedDispatcher that is meant for testing.
class TestSynchronizedDispatcher {
 public:
  TestSynchronizedDispatcher() = default;

  // The dispatcher must be completely shutdown before the dispatcher can be closed.
  // i.e. StopAsync and WaitForStop must be called.
  ~TestSynchronizedDispatcher();

  // Start the dispatcher. Once this returns successfully the dispatcher is available to be
  // used for queueing and running tasks.
  zx::result<> Start(fdf::SynchronizedDispatcher::Options options,
                     std::string_view dispatcher_name);

  // Stop the dispatcher. This must be called before TestSynchronizedDispatcher is destructed.
  // This will block until the dispatcher is stopped, so this cannot be run on the dispatcher's
  // thread.
  zx::result<> StopSync();

  // Request that the dispatcher is stopped. This can be called on the dispatcher's thread.
  // This should be used in conjunction with WaitForStop.
  void StopAsync();

  // Wait until the dispatcher is stopped. This will block the current thread, so it should not be
  // called on the dispatcher's thread.
  zx::result<> WaitForStop();

  const fdf::SynchronizedDispatcher& driver_dispatcher() { return dispatcher_; }
  async_dispatcher_t* dispatcher() { return dispatcher_.async_dispatcher(); }

 private:
  fdf::SynchronizedDispatcher dispatcher_;
  libsync::Completion dispatcher_shutdown_;
};

}  // namespace fdf

#endif  // LIB_DRIVER_RUNTIME_TESTING_RUNTIME_DISPATCHER_H_
