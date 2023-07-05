// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#if __Fuchsia_API_level__ >= 13

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <zircon/assert.h>

namespace fdf {

StartCompleter::StartCompleter(StartCompleter&& other) noexcept
    : complete_(other.complete_), cookie_(other.cookie_), driver_(std::move(other.driver_)) {
  other.complete_ = nullptr;
  other.cookie_ = nullptr;
}

StartCompleter::~StartCompleter() {
  ZX_ASSERT_MSG(complete_ == nullptr, "StartCompleter was not called before going out of scope.");
}

void StartCompleter::operator()(zx::result<> result) {
  ZX_ASSERT_MSG(complete_ != nullptr, "Cannot call StartCompleter more than once.");
  complete_(cookie_, result.status_value(), driver_.release());
  complete_ = nullptr;
  cookie_ = nullptr;
}

}  // namespace fdf

#endif
