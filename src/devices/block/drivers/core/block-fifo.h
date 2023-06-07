// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_CORE_BLOCK_FIFO_H_
#define SRC_DEVICES_BLOCK_DRIVERS_CORE_BLOCK_FIFO_H_

#include <stdint.h>

// LINT.IfChange

// bindgen doesn't like 'using'.
// NOLINTBEGIN(modernize-use-using)
typedef int32_t zx_status_t;
typedef uint32_t reqid_t;
typedef uint16_t groupid_t;
typedef uint16_t vmoid_t;

typedef struct BlockFifoRequest {
  uint32_t opcode;
  reqid_t reqid;
  groupid_t group;
  vmoid_t vmoid;
  uint32_t length;
  uint64_t vmo_offset;
  uint64_t dev_offset;
  uint64_t trace_flow_id;
} block_fifo_request_t;

typedef struct BlockFifoResponse {
  zx_status_t status;
  reqid_t reqid;
  groupid_t group;
  uint16_t padding_to_satisfy_zerocopy;
  uint32_t count;
  uint64_t padding_to_match_request_size_and_alignment[3];
} block_fifo_response_t;

// NOLINTEND(modernize-use-using)

// Notify humans to update Rust bindings because there's no bindgen automation.
// TODO(https://fxbug.dev/73858): Remove lint when no longer necessary.
// LINT.ThenChange(/src/lib/storage/block_client/rust/src/fifo.rs)

#endif  // SRC_DEVICES_BLOCK_DRIVERS_CORE_BLOCK_FIFO_H_
