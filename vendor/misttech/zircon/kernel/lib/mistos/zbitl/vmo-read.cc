// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zbitl/vmo.h>

#include <memory>
#include <new>

#include <ktl/byte.h>
#include <ktl/unique_ptr.h>

#include <ktl/enforce.h>

// The DoRead method goes into a separate translation unit that need not be
// linked in if it's not used.  Callers not using Read checking don't need to
// link in the allocator code at all.

namespace zbitl {

fit::result<zx_status_t> StorageTraits<zx::vmo>::DoRead(const zx::vmo& vmo, uint64_t offset,
                                                        uint32_t length,
                                                        bool (*cb)(void*, ByteView), void* arg) {
  if (length == 0) {
    cb(arg, {});
    return fit::ok();
  }

  // This always copies, when mapping might be better for large sizes.  But
  // address space is cheap, so users concerned with large sizes should just
  // map the whole ZBI in and use View<std::span> instead.
  auto size = [&]() { return ktl::min(static_cast<uint32_t>(kBufferedReadChunkSize), length); };
  fbl::AllocChecker ac;
  ktl::unique_ptr<ktl::byte[]> buf{new (ac) ktl::byte[size()]};
  ZX_ASSERT(ac.check());

  while (length > 0) {
    const uint32_t n = size();
    zx_status_t status = vmo.read(buf.get(), offset, n);
    if (status != ZX_OK) {
      return fit::error{status};
    }
    if (!cb(arg, {buf.get(), n})) {
      break;
    }
    offset += n;
    length -= n;
  }

  return fit::ok();
}

}  // namespace zbitl
