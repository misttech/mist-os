// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/logging/logging.h"

#include <lib/mistos/util/back_insert_iterator.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <object/vm_object_dispatcher.h>

#include <ktl/enforce.h>

namespace starnix {

starnix_uapi::Errno impossible_error(zx_status_t status) {
  PANIC("encountered impossible error: %d", status);
}

fbl::Vector<uint8_t> from_bytes_lossy(ktl::span<const uint8_t> name) {
  fbl::Vector<uint8_t> truncated_name;
  fbl::AllocChecker ac;
  truncated_name.reserve(ZX_MAX_NAME_LEN, &ac);
  ZX_ASSERT(ac.check());

  ktl::transform(name.begin(), name.end(), util::back_inserter(truncated_name),
                 [](uint8_t c) { return c == '\0' ? '?' : c; });

  if (truncated_name.size() > ZX_MAX_NAME_LEN - 1) {
    truncated_name.resize(ZX_MAX_NAME_LEN - 1, &ac);
    ZX_ASSERT(ac.check());
  }
  truncated_name.push_back('\0', &ac);
  ASSERT_MSG(ac.check(), "all the null bytes should have been replace with an escape");

  return ktl::move(truncated_name);
}

void set_zx_name(const zx::object_base& obj, const ktl::span<const uint8_t>& name) {
  auto tname = from_bytes_lossy(name);
  if (zx_status_t s = obj.set_property(ZX_PROP_NAME, tname.data(), tname.size()); s != ZX_OK) {
    impossible_error(s);
  }
}

}  // namespace starnix
