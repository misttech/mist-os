// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_BIND_H_
#define SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_BIND_H_

#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Binds the current process to the provided process registry.
//
// `registry_channel` must be connected to the `fuchsia.memory.heapdump.process.Registry` server.
void heapdump_bind_with_channel(zx_handle_t registry_channel);

// Binds the current process to the process registry, using `fdio_service_connect` to locate it.
void heapdump_bind_with_fdio(void);

__END_CDECLS

#endif  // SRC_PERFORMANCE_MEMORY_HEAPDUMP_INSTRUMENTATION_INCLUDE_HEAPDUMP_BIND_H_
