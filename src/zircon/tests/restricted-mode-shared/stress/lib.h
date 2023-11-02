// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_ZIRCON_TESTS_RESTRICTED_MODE_SHARED_STRESS_LIB_H_
#define SRC_ZIRCON_TESTS_RESTRICTED_MODE_SHARED_STRESS_LIB_H_

#include <lib/zx/time.h>

// This test aims to stress test the shared address region of unified aspaces.
//
// Failures in this test indicate concurrency issues in the shared mappings portion of the unified
// aspace implementation. More often than not, the issue will be in the TLB invalidation logic.
//
// This test performs the following sequence of events on loop:
// 1. The main thread will map a shared address and write a random value to it.
// 2. The main thread will spin up a bunch of reader threads.
// 3. The readers will attempt to read the shared address and pause.
// 4. The main thread will verify that the readers all read the right value.
// 5. The main thread will unmap the shared address and resume all of the readers.
// 6. The readers will then attempt to read the address again and fault.
// 7. The main thread will verify that all the readers faulted.
// 8. The main thread will remap the shared address with a new random value.
// 9. The main thread will resume all of the readers and wait for them to exit.
// 10. The main thread will then verify that the readers all read the new random value.
//
// This loop will continue for `runtime`, and will print out an update with the number of
// iterations completed every `poll_interval`.
void Orchestrator(zx::duration runtime, zx::duration poll_interval);

#endif  // SRC_ZIRCON_TESTS_RESTRICTED_MODE_SHARED_STRESS_LIB_H_
