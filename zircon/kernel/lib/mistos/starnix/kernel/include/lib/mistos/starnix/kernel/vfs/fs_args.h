// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_ARGS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_ARGS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/strings/utf_codecs.h>

#include <charconv>

#include <fbl/intrusive_hash_table.h>
#include <ktl/unique_ptr.h>

namespace starnix::fs_args {

struct HashableFsString : public fbl::SinglyLinkedListable<ktl::unique_ptr<HashableFsString>> {
  // Required to instantiate fbl::DefaultKeyedObjectTraits.
  FsString GetKey() const { return key; }

  // Required to instantiate fbl::DefaultHashTraits.
  static size_t GetHash(FsString key) { return std::hash<std::string_view>{}(key); }

  FsString key;
  FsString value;
};

// generic_parse_mount_options parses a comma-separated list of options of the
// form "key" or "key=value", where neither key nor value contain commas, and
// returns it as a map. If `data` contains duplicate keys, then the last value
// wins. For example:
//
// data = "key0=value0,key1,key2=value2,key0=value3" ->
// map{"key0":"value3","key1":"","key2":"value2"}
//
// generic_parse_mount_options is not appropriate if values may contain commas.
void generic_parse_mount_options(const FsStr& data,
                                 fbl::HashTable<FsString, ktl::unique_ptr<HashableFsString>>* out);

// Parses `data` slice into another type.
//
// This relies on std::from_chars and validade the `data` to be utf8.
template <typename T>
fit::result<Errno, T> parse(const FsString& data) {
  if (!util::IsStringUTF8(data)) {
    return fit::error(errno(EINVAL));
  }
  T parsed_value;
  auto [ptr, ec] = std::from_chars(data.data(), data.data() + data.size(), parsed_value);
  if (ec == std::errc()) {
    return fit::ok(parsed_value);
  }
  return fit::error(errno(EINVAL));
}

}  // namespace starnix::fs_args

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_ARGS_H_
