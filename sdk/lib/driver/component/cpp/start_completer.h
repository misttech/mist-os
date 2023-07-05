// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_
#define LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_

#if __Fuchsia_API_level__ >= 13

#include <lib/driver/symbols/symbols.h>
#include <lib/zx/result.h>

#include <memory>

namespace fdf {

// Forward declare to avoid cycle.
class DriverBase;

// This class wraps the completion of the Start driver lifecycle hook.
// The completer must be called before this class is destroyed. This is a move-only type.
class StartCompleter {
 public:
  explicit StartCompleter(StartCompleteCallback* complete, void* cookie)
      : complete_(complete), cookie_(cookie) {}

  StartCompleter(StartCompleter&& other) noexcept;

  StartCompleter(const StartCompleter&) = delete;
  StartCompleter& operator=(const StartCompleter&) = delete;

  ~StartCompleter();

  // Complete the Start async operation. Safe to call from any thread.
  // Invoking this method will move ownership of `driver_` into the driver framework,
  // extending it's lifetime until the driver's stop hook is invoked. This method is
  // destructive and should not be invoked more than once.
  void operator()(zx::result<> result);

  // This will make the completer take temporary ownership of the driver pointer.
  // It will release its ownership of the driver once the |operator()| has been called.
  // This is used by the internal driver factory. It should NOT be called by the user (driver).
  void set_driver(std::unique_ptr<DriverBase> driver) { driver_ = std::move(driver); }

 private:
  StartCompleteCallback* complete_;
  void* cookie_;

  // The completer will take temporary ownership of the driver pointer.
  // It will release its ownership of the driver once the |operator()| has been called.
  std::unique_ptr<DriverBase> driver_;
};

}  // namespace fdf

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_
