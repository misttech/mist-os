// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_ARGS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_ARGS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/util/strings/utf_codecs.h>

#include <charconv>

#include <fbl/intrusive_hash_table.h>
#include <ktl/optional.h>
#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>

namespace unit_testing {
bool parse_options_with_trailing_comma();
bool parse_options_last_value_wins();
bool parse_options_quoted();
bool parse_options_misquoted();
bool parse_options_misquoted_tail();
bool parse_normal_mount_flags();
bool parse_and_remove_normal_mount_flags();
}  // namespace unit_testing

namespace starnix {

using starnix_uapi::Errno;
using starnix_uapi::MountFlags;
using starnix_uapi::MountFlagsEnum;

struct HashableFsString : public fbl::SinglyLinkedListable<ktl::unique_ptr<HashableFsString>> {
  // Required to instantiate fbl::DefaultKeyedObjectTraits.
  FsString GetKey() const { return key; }

  // Required to instantiate fbl::DefaultHashTraits.
  static size_t GetHash(const FsString& key) { return std::hash<ktl::string_view>{}(key); }

  FsString key;

  FsString value;
};

using FsStringHashTable = fbl::HashTable<FsString, ktl::unique_ptr<HashableFsString>>;

/// Parses a comma-separated list of options of the form `key` or `key=value` or `key="value"`.
/// Commas and equals-signs are only permitted in the `key="value"` case. In the case of
/// `key=value1,key=value2` collisions, the last value wins. Returns a hashmap of key/value pairs,
/// or `EINVAL` in the case of malformed input. Note that no escape character sequence is supported,
/// so values may not contain the `"` character.
///
/// # Examples
///
/// `key0=value0,key1,key2=value2,key0=value3` -> `map{"key0":"value3","key1":"","key2":"value2"}`
///
/// `key0=value0,key1="quoted,with=punc:tua-tion."` ->
/// `map{"key0":"value0","key1":"quoted,with=punc:tua-tion."}`
///
/// `key0="mis"quoted,key2=unquoted` -> `EINVAL`
class MountParams {
 private:
  FsStringHashTable options_;

 public:
  static fit::result<Errno, MountParams> parse(const FsStr& data);

  ktl::optional<FsString> remove(const char* key) {
    auto ptr = options_.erase(key);
    if (ptr) {
      return ptr->value;
    }
    return ktl::nullopt;
  }

  bool is_empty() const { return options_.is_empty(); }

  MountFlags remove_mount_flags() {
    MountFlags flags = MountFlags::empty();
    if (remove("ro")) {
      flags |= MountFlagsEnum::RDONLY;
    }
    if (remove("nosuid")) {
      flags |= MountFlagsEnum::NOSUID;
    }
    if (remove("nodev")) {
      flags |= MountFlagsEnum::NODEV;
    }
    if (remove("noexec")) {
      flags |= MountFlagsEnum::NOEXEC;
    }
    if (remove("noatime")) {
      flags |= MountFlagsEnum::NOATIME;
    }
    if (remove("nodiratime")) {
      flags |= MountFlagsEnum::NODIRATIME;
    }
    if (remove("relatime")) {
      flags |= MountFlagsEnum::RELATIME;
    }
    if (remove("strictatime")) {
      flags |= MountFlagsEnum::STRICTATIME;
    }
    return flags;
  }

  MountParams() = default;
  MountParams(const MountParams& other);

 private:
  friend bool unit_testing::parse_options_with_trailing_comma();
  friend bool unit_testing::parse_options_last_value_wins();
  friend bool unit_testing::parse_options_quoted();
  friend bool unit_testing::parse_options_misquoted();
  friend bool unit_testing::parse_options_misquoted_tail();
  friend bool unit_testing::parse_normal_mount_flags();
  friend bool unit_testing::parse_and_remove_normal_mount_flags();
};

/// Parses `data` slice into another type.
///
/// This relies on str::parse so expects `data` to be utf8.
template <typename T>
fit::result<Errno, T> parse(const FsStr& data) {
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

namespace parse_mount_options {

fit::result<Errno> parse_mount_options(const FsStr& input, FsStringHashTable* out);

}

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_ARGS_H_
