// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_args.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>

#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>

namespace starnix {

namespace parse_mount_options {

fit::result<Errno> parse_mount_options(const FsStr& input, FsStringHashTable* out) {
  if (out == nullptr) {
    return fit::error(errno(EINVAL));
  }

  if (input.empty()) {
    return fit::ok();
  }

  auto start = input.begin();
  auto end = input.end();
  while (start <= end) {
    auto comma_pos = std::find(start, end, ',');
    auto equal_pos = std::find(start, comma_pos, '=');
    size_t value_length = (comma_pos > equal_pos) ? (comma_pos - equal_pos - 1) : 0;

    fbl::AllocChecker ac;
    ktl::unique_ptr<HashableFsString> hashable(new (&ac) HashableFsString{});
    ZX_ASSERT(ac.check());

    hashable->key = ktl::move(FsString(&*start, static_cast<size_t>(equal_pos - start)));
    hashable->value = ktl::move(FsString(&*(equal_pos + 1), value_length));
    out->insert_or_replace(ktl::move(hashable));

    if (comma_pos == end) {
      break;
    }
    start = comma_pos + 1;
  }
  return fit::ok();
}

}  // namespace parse_mount_options

fit::result<Errno, MountParams> MountParams::parse(const FsStr& data) {
  MountParams mp;
  auto options_or_error = parse_mount_options::parse_mount_options(data, &mp.options_);
  if (options_or_error.is_error())
    return options_or_error.take_error();
  return fit::ok(mp);
}

MountParams::MountParams(const MountParams& other) {
  for (const auto& pair : other.options_) {
    fbl::AllocChecker ac;
    ktl::unique_ptr<HashableFsString> hashable(new (&ac) HashableFsString{});
    ZX_ASSERT(ac.check());
    hashable->key = pair.key;
    hashable->value = pair.value;
    options_.insert_or_replace(ktl::move(hashable));
  }
}

}  // namespace starnix
