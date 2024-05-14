// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory_accessor.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>

namespace starnix {

fit::result<Errno, fbl::Vector<uint8_t>> MemoryAccessorExt::read_memory_to_vec(UserAddress addr,
                                                                               size_t len) const {
  return read_to_vec<uint8_t, Errno>(
      len, [&](ktl::span<uint8_t> b) -> fit::result<Errno, NumberOfElementsRead> {
        auto read_result = this->read_memory(addr, b);
        if (read_result.is_error()) {
          return read_result.take_error();
        }
        DEBUG_ASSERT(len == read_result.value().size());
        return fit::ok(NumberOfElementsRead{read_result.value().size()});
      });
}

}  // namespace starnix
