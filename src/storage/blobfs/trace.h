// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_BLOBFS_TRACE_H_
#define SRC_STORAGE_BLOBFS_TRACE_H_

#ifdef __Fuchsia__
#include <lib/trace/event.h>
#else
// Define tracing macros as no-ops for host builds and targets that don't depend on ulib/trace.
#define TRACE_DURATION(args...)
#define TRACE_FLOW_BEGIN(args...)
#define TRACE_FLOW_STEP(args...)
#define TRACE_FLOW_END(args...)
#endif

#endif  // SRC_STORAGE_BLOBFS_TRACE_H_
