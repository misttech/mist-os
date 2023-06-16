// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_ASYNC_TASK_H_
#define LIB_DRIVER_TESTING_CPP_ASYNC_TASK_H_

#include <lib/driver/runtime/testing/cpp/sync_helpers.h>

#include <future>

namespace fdf_testing {

// This class wraps an async task that may require unmanaged dispatchers to run to unblock.
//
// Unmanaged dispatchers are driver dispatchers that are not ran as part of the managed
// driver runtime thread pool, and must therefore be manually told to run. The default dispatcher
// created using |fdf::TestSynchronizedDispatcher| is an unmanaged dispatcher.
// See |fdf::kDispatcherDefault| in `//sdk/lib/driver/runtime/testing/cpp/dispatcher.h` for more.
//
// AsyncTask therefore provides a way to await the result of the task while running unmanaged
// driver dispatchers (if there are any) on the calling thread to ensure the task can complete.
template <typename ResultType>
class [[nodiscard]] AsyncTask {
 public:
  explicit AsyncTask(std::shared_future<ResultType> future) : future_(std::move(future)) {}

  // Await the completion of the task while running unmanaged dispatchers if there are any.
  // Returns the result of the async task when complete.
  //
  // This MUST be called from the main test thread.
  ResultType Await() {
    ZX_ASSERT_MSG(future_.valid(), "Invalid future, cannot call await more than once.");
    zx::result result = fdf::WaitFor(std::move(future_));
    ZX_ASSERT_MSG(ZX_OK == result.status_value(), "Call to fdf::WaitFor failed.");
    return result.value();
  }

 private:
  std::shared_future<ResultType> future_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_ASYNC_TASK_H_
