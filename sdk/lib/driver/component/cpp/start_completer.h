// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_
#define LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_

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

  void set_driver(std::unique_ptr<DriverBase> driver) { driver_ = std::move(driver); }

  // Complete the Start async operation. Safe to call from any thread.
  void operator()(zx::result<> result);

 private:
  StartCompleteCallback* complete_;
  void* cookie_;
  std::unique_ptr<DriverBase> driver_;
};

}  // namespace fdf

#endif  // LIB_DRIVER_COMPONENT_CPP_START_COMPLETER_H_
