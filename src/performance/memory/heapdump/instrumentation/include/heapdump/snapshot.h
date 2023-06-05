// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_SNAPSHOT_H_
#define SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_SNAPSHOT_H_

#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Publishes a named snapshot of all the current live allocations.
void heapdump_take_named_snapshot(const char *snapshot_name);

__END_CDECLS

#endif  // SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_SNAPSHOT_H_
