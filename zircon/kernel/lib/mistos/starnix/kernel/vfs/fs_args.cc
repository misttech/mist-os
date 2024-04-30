// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_args.h"

#include <ktl/unique_ptr.h>


namespace starnix::fs_args {

void generic_parse_mount_options(const FsStr& data,
                                 fbl::HashTable<FsString, ktl::unique_ptr<HashableFsString>>* out) {
  if (data.empty() || out == nullptr) {
    return;
  }

  auto start = data.begin();
  auto end = data.end();
  while (start <= end) {
    auto comma_pos = std::find(start, end, ',');
    auto equal_pos = std::find(start, comma_pos, '=');
    size_t value_length = (comma_pos > equal_pos) ? (comma_pos - equal_pos - 1) : 0;

    FsString key(start, equal_pos - start);
    FsString value(equal_pos + 1, value_length);

    fbl::AllocChecker ac;
    ktl::unique_ptr<HashableFsString> hashable(new (&ac) HashableFsString{});
    ZX_ASSERT(ac.check());
    hashable->key = key;
    hashable->value = value;
    out->insert_or_replace(std::move(hashable));

    if (comma_pos == end) {
      break;
    }
    start = comma_pos + 1;
  }
}

} // namespace starnix::fs_args
