// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_uapi/signals.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>

namespace starnix_uapi {

bool Signal::is_unblockable() const { return UNBLOCKABLE_SIGNALS.has_signal(*this); }

fit::result<Errno, Signal> Signal::try_from(UncheckedSignal _value) {
  uint32_t value = static_cast<uint32_t>(_value.value());
  if (value >= 1 && value <= Signal::NUM_SIGNALS) {
    return fit::ok(Signal(value));
  }
  return fit::error(errno(EINVAL));
}

}  // namespace starnix_uapi
