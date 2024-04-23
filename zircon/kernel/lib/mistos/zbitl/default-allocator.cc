// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zbitl/decompress.h>

#include <ktl/byte.h>
#include <ktl/move.h>

namespace zbitl {
namespace decompress {

fit::result<std::string_view, std::unique_ptr<ktl::byte[]>> DefaultAllocator(size_t bytes) {
  if (auto ptr = std::make_unique<ktl::byte[]>(bytes)) {
    return fit::ok(ktl::move(ptr));
  }
  return fit::error{"out of memory"};
}

}  // namespace decompress
}  // namespace zbitl
