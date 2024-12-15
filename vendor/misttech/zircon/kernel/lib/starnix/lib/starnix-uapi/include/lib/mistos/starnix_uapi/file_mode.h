// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_FILE_MODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_FILE_MODE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/bitflags.h>
#include <lib/mistos/util/strings/utf_codecs.h>
#include <zircon/types.h>

#include <charconv>

#include <linux/stat.h>

#define FILE_MODE(type, mode) \
  starnix_uapi::FileMode::from_bits(mode | starnix_uapi::FileMode::type.bits())

namespace starnix_uapi {

class FileMode {
 public:
  static const FileMode IFLNK;
  static const FileMode IFREG;
  static const FileMode IFDIR;
  static const FileMode IFCHR;
  static const FileMode IFBLK;
  static const FileMode IFIFO;
  static const FileMode IFSOCK;

  static const FileMode ISUID;
  static const FileMode ISGID;
  static const FileMode ISVTX;
  static const FileMode IRWXU;
  static const FileMode IRUSR;
  static const FileMode IWUSR;
  static const FileMode IXUSR;
  static const FileMode IRWXG;
  static const FileMode IRGRP;
  static const FileMode IWGRP;
  static const FileMode IXGRP;
  static const FileMode IRWXO;
  static const FileMode IROTH;
  static const FileMode IWOTH;
  static const FileMode IXOTH;

  static const FileMode IFMT;

  static const FileMode DEFAULT_UMASK;
  static const FileMode ALLOW_ALL;
  static const FileMode PERMISSIONS;
  static const FileMode EMPTY;

  FileMode(uint32_t mode) : mode_(mode) {}
  FileMode() : mode_(0) {}

  static FileMode from_bits(uint32_t mask) { return FileMode(mask); }

  static fit::result<Errno, FileMode> from_string(const std::string_view& mask) {
    if (mask.size() > 1 && mask[0] != '0') {
      return fit::error(errno(EINVAL));
    }

    if (!util::IsStringUTF8(mask)) {
      return fit::error(errno(EINVAL));
    }

    uint32_t bits = 0;
    // Skip the '0' prefix
    std::string_view octal_string = mask.substr(1);
    auto [ptr, ec] =
        std::from_chars(octal_string.data(), octal_string.data() + octal_string.size(), bits, 8);
    if (ec == std::errc()) {
      return fit::ok(FileMode::from_bits(bits));
    }
    return fit::error(errno(EINVAL));
  }

  uint32_t bits() const { return mode_; }
  bool contains(const FileMode& other) const { return (mode_ & other.mode_) == other.mode_; }
  bool intersects(const FileMode& other) const { return (mode_ & other.mode_) != 0; }

  FileMode fmt() const { return FileMode(bits() & S_IFMT); }

  FileMode with_type(const FileMode& file_type) const {
    return FileMode((mode_ & PERMISSIONS.bits()) | (file_type.bits() & S_IFMT));
  }

  bool is_lnk() const { return (mode_ & S_IFMT) == S_IFLNK; }
  bool is_reg() const { return (mode_ & S_IFMT) == S_IFREG; }
  bool is_dir() const { return (mode_ & S_IFMT) == S_IFDIR; }
  bool is_chr() const { return (mode_ & S_IFMT) == S_IFCHR; }
  bool is_blk() const { return (mode_ & S_IFMT) == S_IFBLK; }
  bool is_fifo() const { return (mode_ & S_IFMT) == S_IFIFO; }
  bool is_sock() const { return (mode_ & S_IFMT) == S_IFSOCK; }

  bool operator!=(const FileMode& other) const { return mode_ != other.mode_; }
  bool operator==(const FileMode& other) const { return mode_ == other.mode_; }

  FileMode operator&(const FileMode& other) const { return mode_ & other.mode_; }
  FileMode& operator&=(const FileMode& other) {
    mode_ &= other.mode_;
    return *this;
  }
  FileMode operator|(const FileMode& other) const { return mode_ | other.mode_; }
  FileMode& operator|=(const FileMode& other) {
    mode_ |= other.mode_;
    return *this;
  }
  FileMode operator~() const { return ~mode_; }

 private:
  uint32_t mode_;
};

namespace inner_access {

enum class AccessEnum : uint32_t {
  EXIST = 0,
  EXEC = 1,
  WRITE = 2,
  READ = 4,

  // Access mask is the part of access related to the file access mode. It is
  // exec/write/read.
  ACCESS_MASK = 1 | 2 | 4,
};

using AccessFlags = Flags<AccessEnum>;

class Access : public AccessFlags {
 public:
  explicit Access(AccessFlags flag) : AccessFlags(flag) {}
  explicit Access(AccessEnum value) : AccessFlags(value) {}

  static Access from_open_flags(OpenFlags flags) {
    auto access_mask = flags & OpenFlagsEnum::ACCESS_MASK;
    if (access_mask.to_enum() == OpenFlagsEnum::RDONLY) {
      return Access(AccessEnum::READ);
    } else if (access_mask.to_enum() == OpenFlagsEnum::WRONLY) {
      return Access(AccessEnum::WRITE);
    } else if (access_mask.to_enum() == OpenFlagsEnum::RDWR) {
      return Access(Access(AccessEnum::READ) | Access(AccessEnum::WRITE));

    } else {
      // Nonstandard access modes can be opened but will fail to read or write
      return Access(AccessEnum::EXIST);
    }
  }

  // impl Access
  static Access rwx() {
    return Access(Access(AccessEnum::EXEC) | Access(AccessEnum::WRITE) | Access(AccessEnum::READ));
  }

  bool is_nontrivial() const { return *this != Access(AccessEnum::EXIST); }

  uint32_t rwx_bits() const { return bits() & Access(AccessEnum::ACCESS_MASK).bits(); }
};

}  // namespace inner_access

using Access = inner_access::Access;
using AccessEnum = inner_access::AccessEnum;

}  // namespace starnix_uapi

template <>
constexpr Flag<starnix_uapi::inner_access::AccessEnum>
    Flags<starnix_uapi::inner_access::AccessEnum>::FLAGS[] = {
        {starnix_uapi::inner_access::AccessEnum::EXIST},
        {starnix_uapi::inner_access::AccessEnum::EXEC},
        {starnix_uapi::inner_access::AccessEnum::WRITE},
        {starnix_uapi::inner_access::AccessEnum::READ},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_FILE_MODE_H_
