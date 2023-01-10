// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_LIB_STORAGE_STORAGE_H_
#define SRC_FIRMWARE_LIB_STORAGE_STORAGE_H_

#ifdef FIRMWARE_STORAGE_CUSTOM_SYSDEPS_HEADER
#include <firmware_storage_sysdeps.h>
#else
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

// The macro specifies the buffer alignment requirement for reading/writing device storage. One
// example is DMA alignment. Users should define this according to the actual device. The default
// value is 64.
#ifndef FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT
#define FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT 64
#endif

typedef struct FuchsiaFirmwareStorage {
  // The minimal size for read/write. On EMMC this should be the block size. On NAND this should be
  // the page size.
  size_t block_size;

  // The total size of the storage in number of blocks.
  size_t total_blocks;

  // A scratch buffer for unaligned read/write. It must have a capacity at least `block_size` and
  // must be buffer aligned according to FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT.
  void* scratch_buffer;

  // Context pointer for calling the `read` callback.
  void* ctx;

  // Callback for reading from the storage.
  //
  // Implementation can assume that`dst` is buffer aligned according to
  // FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT. However `block_offset` and `blocks_count` must be
  // treated as logical. It's up to the implementation to convert them into physical offset and size
  // for accessing storage. For example, in the case of NAND device, implementation must take into
  // consideration bad block and may want to compute the corresponding physical offset and size
  // using skip block logic.
  //
  // @ctx: Context pointer. It will always be the `ctx` field above.
  // @block_offset: Logical offset from the storage to read in terms of number of blocks.
  // @blocks_count: Logical size to read in terms of number of blocks.
  // @dst: Output pointer to read to.
  //
  // Return true if success, false otherwise.
  bool (*read)(void* ctx, size_t block_offset, size_t blocks_count, void* dst);

  // Callback for writing to the storage.
  //
  // Similar to read(), implementation can assume that `src` is buffer aligned according to
  // FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT. However `block_offset` and `blocks_count` must be
  // treated as logical.
  //
  // @ctx: Context pointer. It will always be the `ctx` field above.
  // @block_offset: Logical offset from the storage to write in terms of number of blocks.
  // @blocks_count: Logical size to write in terms of number of blocks.
  // @src: Pointer to buffer of data to write.
  //
  // Return true if success, false otherwise.
  bool (*write)(void* ctx, size_t block_offset, size_t blocks_count, const void* src);
} FuchsiaFirmwareStorage;

// Read data from storage
//
// @ops: Pointer to FuchsiaFirmwareStorage
// @offset: offset in number of bytes to read from.
// @size: size in number of bytes to read.
// @dst: Pointer to buffer to read to.
//
// Return true if success, false otherwise.
bool FuchsiaFirmwareStorageRead(FuchsiaFirmwareStorage* ops, size_t offset, size_t size, void* dst);

#ifdef __cplusplus
}
#endif

#endif  // SRC_FIRMWARE_LIB_STORAGE_STORAGE_H_
