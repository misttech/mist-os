// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "storage.h"

#ifdef ENABLE_FIRMWARE_STORAGE_LOG
#define firmware_storage_log printf
#else
#define firmware_storage_log(...)
#endif

#define FIRMWARE_STORAGE_MIN(a, b) (((a) < (b)) ? (a) : (b))
#define FIRMWARE_STORAGE_MAX(a, b) (((a) > (b)) ? (a) : (b))
#define FIRMWARE_STORAGE_IS_ALIGNED(a, b) ((((uintptr_t)(a)) % (uintptr_t)(b)) == 0)

#define FIRMWARE_STORAGE_ROUNDUP(a, b) \
  ({                                   \
    const __typeof((a)) _a = (a);      \
    const __typeof((b)) _b = (b);      \
    ((_a + _b - 1) / _b * _b);         \
  })

#define FIRMWARE_STORAGE_ROUNDDOWN(a, b) \
  ({                                     \
    const __typeof((a)) _a = (a);        \
    const __typeof((b)) _b = (b);        \
    _a - (_a % _b);                      \
  })

static bool ReadWrapper(FuchsiaFirmwareStorage *ops, size_t offset, size_t size, void *dst) {
  size_t offset_blocks = offset / ops->block_size;
  size_t size_blocks = size / ops->block_size;
  if (offset_blocks + size_blocks > ops->total_blocks) {
    firmware_storage_log("Read overflow\n");
    return false;
  }
  return ops->read(ops->ctx, offset_blocks, size_blocks, dst);
}

// First, relax the `size` alignment requirement, assume aligned offset and buffer:
// offset
//   |~~~~~~~~~size~~~~~~~~~|
//   |---------|---------|---------|
static bool ReadWithAlignedOffsetAndDma(FuchsiaFirmwareStorage *ops, size_t offset, size_t size,
                                        void *dst) {
  // Read the aligned part.
  size_t aligned_read_size = FIRMWARE_STORAGE_ROUNDDOWN(size, ops->block_size);
  if (aligned_read_size && !ReadWrapper(ops, offset, aligned_read_size, dst)) {
    firmware_storage_log("Failed to read aligned part\n");
    return false;
  }

  if (aligned_read_size == size) {
    return true;
  }

  // Read the remaining unaligned part into the scratch buffer
  if (!ReadWrapper(ops, offset + aligned_read_size, ops->block_size, ops->scratch_buffer)) {
    firmware_storage_log("Failed to read unaligned part\n");
    return false;
  }
  memcpy((uint8_t *)dst + aligned_read_size, ops->scratch_buffer, size - aligned_read_size);
  return true;
}

// Second, relax both the `offset` and `size` alignment requirement, assume aligned buffer.
bool ReadWithAlignedDma(FuchsiaFirmwareStorage *ops, size_t offset, size_t size, void *dst) {
  if (FIRMWARE_STORAGE_IS_ALIGNED(offset, ops->block_size)) {
    return ReadWithAlignedOffsetAndDma(ops, offset, size, dst);
  }
  size_t aligned_offset = FIRMWARE_STORAGE_ROUNDDOWN(offset, ops->block_size);
  // Case 1:
  //            |~~unaligned_read~~|
  //            |~~~~~~~~~~~~~size~~~~~~~~~~~~|
  //         offset
  //        |----------------------|---------------------|
  //   aligned_offset
  //
  // Case 2:
  //            |~~unaligned_read~~|
  //            |~~~~~~~~size~~~~~~|
  //         offset
  //        |-------------------------|------------------------|
  //   aligned_offset
  size_t unaligned_read = FIRMWARE_STORAGE_MIN(size, ops->block_size - (offset - aligned_offset));
  size_t new_offset = offset + unaligned_read;
  size_t new_size = size - unaligned_read;

  // Read the aligned part first, if there is any, so that we can safely reuse the scratch buffer.
  if (new_size) {
    uint8_t *next = (uint8_t *)dst + unaligned_read;
    // If the new output address is still buffer aligned, read into it.
    if (FIRMWARE_STORAGE_IS_ALIGNED(next, FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT)) {
      if (!ReadWithAlignedOffsetAndDma(ops, new_offset, new_size, next)) {
        return false;
      }
    } else {
      // Otherwise, read into `dst` which is assumed buffer aligned and memmove to correct position.
      if (!ReadWithAlignedOffsetAndDma(ops, new_offset, new_size, dst)) {
        return false;
      }
      memmove(next, dst, new_size);
    }
  }

  // Read the unaligned part to scratch buffer.
  if (!ReadWrapper(ops, aligned_offset, ops->block_size, ops->scratch_buffer)) {
    firmware_storage_log("Failed to read unaligned part\n");
    return false;
  }
  memcpy(dst, ops->scratch_buffer + (offset - aligned_offset), unaligned_read);
  return true;
}

static uint8_t dma_aligned_scratch_buffer[FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT]
    __attribute__((__aligned__(FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT)));

// Finally, relax all of `offset`, `size` and buffer alignment requirement
bool FuchsiaFirmwareStorageRead(FuchsiaFirmwareStorage *ops, size_t offset, size_t size,
                                void *dst) {
  uintptr_t dst_addr = (uintptr_t)dst;
  if (FIRMWARE_STORAGE_IS_ALIGNED(dst_addr, FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT)) {
    return ReadWithAlignedDma(ops, offset, size, dst);
  }
  // Case 1:
  //     |~~~~~~~to_read~~~~~~|
  //     |~~~~~~~~~~~~~dst buffer~~~~~~~~~~~~|
  //   |----------------------|---------------------|
  //     buffer alignment size
  //
  // Case 2:
  //    |~~~~to_read~~~~~~|
  //    |~~~~dst buffer~~~|
  //  |----------------------|---------------------|
  //    buffer alignment size
  size_t to_read = FIRMWARE_STORAGE_MIN(
      size,
      FIRMWARE_STORAGE_ROUNDUP(dst_addr, FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT) - dst_addr);
  if (!ReadWithAlignedDma(ops, offset, to_read, dma_aligned_scratch_buffer)) {
    return false;
  }
  memcpy(dst, dma_aligned_scratch_buffer, to_read);
  return (to_read == size) ||
         ReadWithAlignedDma(ops, offset + to_read, size - to_read, (uint8_t *)dst + to_read);
}
