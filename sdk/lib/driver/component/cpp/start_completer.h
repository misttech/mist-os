// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_
#define LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_

#include <zircon/availability.h>

#if __Fuchsia_API_level__ >= 13

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
#include <lib/fit/function.h>
#else
#include <lib/driver/symbols/symbols.h>

#include <memory>
#endif
#include <lib/zx/result.h>

namespace fdf {

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
// This class is a wrapper for a callback type that must be called into exactly once
// before destruction. It is a move only type.
class Completer {
 public:
  Completer(fit::callback<void(zx::result<>)> callback) : callback_(std::move(callback)) {}

  Completer(Completer&& other) noexcept : callback_(std::move(other.callback_)) {
    other.callback_ = std::nullopt;
  }

  Completer(const Completer&) = delete;
  Completer& operator=(const Completer&) = delete;

  ~Completer();

  // Calls the wrapped callback function.
  // This method should not be invoked more than once.
  void operator()(zx::result<> result);

 private:
  std::optional<fit::callback<void(zx::result<>)>> callback_;
};

// This is the completer for the Start operation in |DriverBase|.
class StartCompleter : public Completer {
 public:
  using Completer::Completer;
  using Completer::operator();
};
#else
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
#endif

}  // namespace fdf

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_
