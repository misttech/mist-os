// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_uapi/math.h"

#include <lib/fit/result.h>
#include <zircon/compiler.h>

#include <arch/defines.h>

namespace starnix_uapi {

fit::result<Errno, size_t> round_up_to_increment(size_t size, size_t increment) {
  auto spare = size % increment;
  if (spare > 0) {
    if (add_overflow(size, increment - spare, &size)) {
      return fit::error(errno(EINVAL));
    }
  }
  return fit::ok(size);
}

fit::result<Errno, size_t> round_up_to_system_page_size(size_t size) {
  return round_up_to_increment(size, PAGE_SIZE);
}

}  // namespace starnix_uapi
