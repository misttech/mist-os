// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zbitl/decompress.h>

#include <fbl/alloc_checker.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/unique_ptr.h>

#include <ktl/enforce.h>

namespace zbitl::decompress {

fit::result<ktl::string_view, ktl::unique_ptr<ktl::byte[]>> DefaultAllocator(size_t bytes) {
  fbl::AllocChecker ac;
  auto ptr = ktl::make_unique<ktl::byte[]>(&ac, bytes);
  if (ac.check()) {
    return fit::ok(ktl::move(ptr));
  }
  return fit::error{"out of memory"};
}

}  // namespace zbitl::decompress
