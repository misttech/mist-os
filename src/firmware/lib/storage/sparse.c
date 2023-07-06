// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/sparse/c/sparse.h>
#include <lib/storage/gpt_utils.h>
#include <lib/storage/sparse.h>
#include <sparse_format.h>
#include <string.h>

#include "lib/storage/storage.h"

#ifdef ENABLE_FIRMWARE_STORAGE_LOG
#define sparse_logger firmware_storage_log
#else
#define sparse_logger sparse_nop_logger
#endif

#define kScratchSize 4096

typedef struct IoBuffer {
  uint8_t* data;
  size_t size;
} IoBuffer;

static size_t IoBufferSize(SparseIoBufferHandle handle) {
  IoBuffer* buffer = (IoBuffer*)(handle);
  return buffer->size;
}

static bool IoBufferRead(SparseIoBufferHandle handle, uint64_t offset, uint8_t* dst, size_t size) {
  IoBuffer* buffer = (IoBuffer*)(handle);
  if (offset + size > buffer->size) {
    return false;
  }
  memcpy(dst, buffer->data + offset, size);
  return true;
}

static bool IoBufferWrite(SparseIoBufferHandle handle, uint64_t offset, const uint8_t* src,
                          size_t size) {
  IoBuffer* buffer = (IoBuffer*)(handle);
  if (offset + size > buffer->size) {
    return false;
  }
  memcpy(buffer->data + offset, src, size);
  return true;
}

typedef struct IoContext {
  FuchsiaFirmwareStorage* ops;
  const GptData* gpt_data;
  const char* name;
} IoContext;

static bool IoWrite(void* ctx, uint64_t device_offset, SparseIoBufferHandle src,
                    uint64_t src_offset, size_t size) {
  IoContext* io = (IoContext*)(ctx);
  IoBuffer* src_buffer = (IoBuffer*)(src);
  if (src_offset + size > src_buffer->size) {
    return false;
  }
  return FuchsiaFirmwareStorageGptWrite(io->ops, io->gpt_data, io->name, device_offset, size,
                                        src_buffer->data + src_offset);
}

bool FuchsiaIsSparseImage(const uint8_t* src, size_t size) {
  static SparseIoBufferOps handle_interface = {
      .size = IoBufferSize,
      .read = IoBufferRead,
      .write = IoBufferWrite,
  };
  IoBuffer src_buffer = {
      .size = size,
      .data = (uint8_t*)src,
  };
  return sparse_is_sparse_image(&handle_interface, &src_buffer);
}

bool FuchsiaWriteSparseImage(FuchsiaFirmwareStorage* ops, const GptData* gpt_data, const char* name,
                             uint8_t* src, size_t size) {
  IoContext context = {ops, gpt_data, name};
  static uint8_t scratch[kScratchSize] = {};
  static IoBuffer scratch_buffer = {
      .size = kScratchSize,
      .data = scratch,
  };
  SparseIoBufferOps handle_ops = {
      .size = IoBufferSize,
      .read = IoBufferRead,
      .write = IoBufferWrite,
  };
  SparseIoInterface io = {
      .ctx = &context,
      .scratch_handle = &scratch_buffer,
      .handle_ops = handle_ops,
      .write = IoWrite,
  };
  IoBuffer src_buffer = {
      .size = size,
      .data = src,
  };
  return sparse_unpack_image(&io, sparse_logger, &src_buffer);
}
