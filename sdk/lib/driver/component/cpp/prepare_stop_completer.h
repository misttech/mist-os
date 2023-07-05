// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_
#define LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_

#if __Fuchsia_API_level__ >= 13

#include <lib/driver/symbols/symbols.h>
#include <lib/zx/result.h>

namespace fdf {

// This class wraps the completion of the PrepareStop driver lifecycle hook.
// The completer must be called before this class is destroyed. This is a move-only type.
class PrepareStopCompleter {
 public:
  explicit PrepareStopCompleter(PrepareStopCompleteCallback* complete, void* cookie)
      : complete_(complete), cookie_(cookie) {}

  PrepareStopCompleter(PrepareStopCompleter&& other) noexcept;

  PrepareStopCompleter(const PrepareStopCompleter&) = delete;
  PrepareStopCompleter& operator=(const PrepareStopCompleter&) = delete;

  ~PrepareStopCompleter();

  // Complete the PrepareStop async operation. Safe to call from any thread.
  void operator()(zx::result<> result);

 private:
  PrepareStopCompleteCallback* complete_;
  void* cookie_;
};

}  // namespace fdf

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_
