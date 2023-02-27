// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_BIND_WITH_FDIO_H_
#define SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_BIND_WITH_FDIO_H_

#include <zircon/compiler.h>

__BEGIN_CDECLS

// Binds the current process to the process registry.
void heapdump_bind_with_fdio(void);

__END_CDECLS

#endif  // SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_BIND_WITH_FDIO_H_
