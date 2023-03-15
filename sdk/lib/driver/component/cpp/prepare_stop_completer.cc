// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <zircon/assert.h>

namespace fdf {

PrepareStopCompleter::PrepareStopCompleter(PrepareStopCompleter&& other) noexcept
    : complete_(other.complete_), cookie_(other.cookie_) {
  other.complete_ = nullptr;
  other.cookie_ = nullptr;
}

PrepareStopCompleter::~PrepareStopCompleter() {
  if (complete_) {
    ZX_ASSERT_MSG(called_, "PrepareStopCompleter was not called before going out of scope.");
  }
}

void PrepareStopCompleter::operator()(zx::result<> result) {
  ZX_ASSERT_MSG(!called_, "Cannot call PrepareStopCompleter more than once.");
  complete_(cookie_, result.status_value());
  called_ = true;
}

}  // namespace fdf
