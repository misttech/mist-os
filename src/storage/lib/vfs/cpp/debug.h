// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_DEBUG_H_
#define SRC_STORAGE_LIB_VFS_CPP_DEBUG_H_

// Debug-only header defining utility functions for logging flags and strings.
// May be used on both Fuchsia and host-only builds.

#ifdef FS_TRACE_DEBUG_ENABLED
#include <fidl/fuchsia.io/cpp/natural_ostream.h>
#include <fidl/fuchsia.io/cpp/type_conversions.h>
#include <fidl/fuchsia.io/cpp/wire.h>

#include <cstdlib>
#include <iostream>
#include <string_view>
#include <type_traits>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"

std::ostream& operator<<(std::ostream& os, const fs::VnodeConnectionOptions& options);
std::ostream& operator<<(std::ostream& os, fs::CreationType type);
namespace fs::debug_internal {

inline void PrintEach(std::ostream& stream) {}

template <typename T, typename... Args>
void PrintEach(std::ostream& stream, T val, Args... args) {
  if constexpr (std::is_same_v<T, const char*> || std::is_same_v<T, std::string_view> ||
                std::is_same_v<T, fs::VnodeConnectionOptions>) {
    stream << val;
  } else if constexpr (std::is_same_v<T, fidl::StringView>) {
    stream << val.get();
  } else if constexpr (fidl::IsWire<T>()) {
    stream << fidl::ostream::Formatted(fidl::ToNatural(val));
  } else {
    stream << fidl::ostream::Formatted(val);
  }
  PrintEach(stream, args...);
  stream << "\n";
}

}  // namespace fs::debug_internal

#define FS_PRETTY_TRACE_DEBUG(args...)              \
  do {                                              \
    fs::debug_internal::PrintEach(std::cerr, args); \
  } while (0)
#else
#define FS_PRETTY_TRACE_DEBUG(args...) \
  do {                                 \
  } while (0)

#endif
#endif  // SRC_STORAGE_LIB_VFS_CPP_DEBUG_H_
