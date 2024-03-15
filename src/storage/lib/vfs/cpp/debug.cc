// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/debug.h"

#ifdef FS_TRACE_DEBUG_ENABLED

std::ostream& operator<<(std::ostream& os, const fs::VnodeConnectionOptions& options) {
  os << "VnodeConnectionOptions{ flags: " << fidl::ostream::Formatted(options.flags)
     << ", rights: " << fidl::ostream::Formatted(options.rights) << "}";
  return os;
}

#endif
