// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_COMMON_INCLUDE_COMMON_H_
#define SRC_DEVICES_BLOCK_LIB_COMMON_INCLUDE_COMMON_H_

#include <zircon/types.h>

namespace block {

// Check whether the IO request fits within the block device.
template <typename T>
inline zx_status_t CheckIoRange(const T& io, uint64_t total_block_count) {
  if (io.length == 0 || io.length > total_block_count) {
    TRACEF("IO request length (%u blocks) is zero or exceeds the total block count (%lu).\n",
           io.length, total_block_count);
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (io.offset_dev >= total_block_count || io.offset_dev > total_block_count - io.length) {
    TRACEF(
        "IO request offset (%lu blocks) and length (%u blocks) does not fit within the total "
        "block count (%lu).\n",
        io.offset_dev, io.length, total_block_count);
    return ZX_ERR_OUT_OF_RANGE;
  }
  return ZX_OK;
}

// Check whether the IO request fits within the block device.
// Also check that the IO request length does not exceed the max transfer size.
template <typename T>
inline zx_status_t CheckIoRange(const T& io, uint64_t total_block_count,
                                uint32_t max_tranfser_blocks) {
  if (io.length > max_tranfser_blocks) {
    TRACEF("IO request length (%u blocks) exceeds max transfer size (%u blocks).\n", io.length,
           max_tranfser_blocks);
    return ZX_ERR_OUT_OF_RANGE;
  }
  return CheckIoRange(io, total_block_count);
}

#if 0
// Check that the data arguments are cleared for a flush request.
inline zx_status_t CheckFlushValid(const block_read_write& rw) {
  if (rw.vmo || rw.length || rw.offset_dev || rw.offset_vmo) {
    TRACEF(
        "Flush request has data arguments: rw.vmo = %u, rw.length = %u, rw.offset_dev = %lu, "
        "rw.offset_vmo = %lu.\n",
        rw.vmo, rw.length, rw.offset_dev, rw.offset_vmo);
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}
#endif

}  // namespace block

#endif  // SRC_DEVICES_BLOCK_LIB_COMMON_INCLUDE_COMMON_H_
