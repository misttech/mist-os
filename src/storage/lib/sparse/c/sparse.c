// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sparse.h"

#include <sparse_format.h>
#include <stdbool.h>
#include <string.h>

// Can't reliably depend on endian macros, so just define them ourselves
#ifndef __BYTE_ORDER__
#error Compiler does not provide __BYTE_ORDER__
#endif

// The compiler provides it, use what it says
#ifndef LITTLE_ENDIAN
#define LITTLE_ENDIAN __ORDER_LITTLE_ENDIAN__
#endif

#ifndef BIG_ENDIAN
#define BIG_ENDIAN __ORDER_BIG_ENDIAN__
#endif

#ifndef BYTE_ORDER
#define BYTE_ORDER __BYTE_ORDER__
#endif

// We only want macros to convert from little-endian
// because that's what the fields in a sparse header are defined as.
#if BYTE_ORDER == BIG_ENDIAN
static inline uint64_t SWAP_64(uint64_t x) { return __builtin_bswap64(x); }
static inline uint32_t SWAP_32(uint32_t x) { return __builtin_bswap32(x); }
static inline uint16_t SWAP_16(uint16_t x) { return __builtin_bswap16(x); }

#define LE64(val) SWAP_64(val)
#define LE32(val) SWAP_32(val)
#define LE16(val) SWAP_16(val)
#else
#define LE64(val) (val)
#define LE32(val) (val)
#define LE16(val) (val)
#endif

#define kMaxBlkSize 4096
#define kFillInts (kMaxBlkSize / sizeof(uint32_t))

static void DumpStruct(SparseLogFn log, const void* s, size_t size) {
#ifdef ENABLE_VERBOSE_SPARSE_LOG
  for (size_t i = 0; i < size; i++) {
    log("0x%02X ", ((uint8_t*)s)[i]);
  }
#endif
  log("\n");
}

// Check the values of the fields in header to make sure they conform to
// the expected semantics of a valid sparse file header.
//
// @header: The sparse header to be checked.
//
// Return true if it is valid, false otherwise.
static bool IsValidFileHeader(const sparse_header_t* header) {
  return header && LE64(header->magic) == SPARSE_HEADER_MAGIC &&
         LE16(header->major_version) <= 0x1 && sizeof(*header) <= LE16(header->file_hdr_sz) &&
         sizeof(chunk_header_t) <= LE16(header->chunk_hdr_sz) && LE32(header->blk_sz) % 4 == 0 &&
         // This is a hack to support FILL chunks via a static buffer.
         // Online documentation implies that blk_size is ALWAYS 4096,
         // but if a flaky or malicious image were crafted with a larger size,
         // this would constitute a buffer overflow and would write unexpected memory to disk.
         // Instead of dynamically allocating a buffer or adding a complex scheme to handle
         // arbitrarily sized blocks via a constant sized buffer, just fail on unlikely input.
         LE32(header->blk_sz) <= kMaxBlkSize;
}

// Check that a sparse image header is valid, i.e. fields are set to reasonable values,
// and if it is valid copy the header to an aligned header struct.
//
// Returns true if the header is valid, false otherwise.
// The header output parameter is only valid if the function returns true.
static bool ValidateHeader(SparseIoBufferOps* handle_ops, SparseIoBufferHandle handle,
                           sparse_header_t* header) {
  return handle_ops->read(handle, 0, (uint8_t*)header, sizeof(*header)) &&
         IsValidFileHeader(header);
}

static bool ValidateChunkSize(const sparse_header_t* header, const chunk_header_t* chunk,
                              size_t payload_size) {
  return LE32(chunk->total_sz) == header->chunk_hdr_sz + payload_size;
}

int sparse_nop_logger(const char* msg, ...) { return 0; }

bool sparse_is_sparse_image(SparseIoBufferOps* handle_ops, SparseIoBufferHandle src) {
  if (handle_ops == NULL) {
    return false;
  }
  sparse_header_t header;
  return ValidateHeader(handle_ops, src, &header);
}

bool sparse_unpack_image(SparseIoInterface* io, SparseLogFn log, SparseIoBufferHandle src) {
  if (io == NULL || log == NULL) {
    return false;
  }
  static uint32_t fill_data[kFillInts] = {};
  static bool first_fill = true;

  const size_t size = io->handle_ops.size(src);

  if (io->handle_ops.size(io->scratch_handle) < sizeof(fill_data)) {
    log("Scratch buffer too small, need %zu bytes\n", sizeof(fill_data));
    return false;
  }

  sparse_header_t file_header;
  if (!io->handle_ops.read(src, 0, (uint8_t*)(&file_header), sizeof(sparse_header_t))) {
    log("Failed to read image header\n");
    return false;
  }
  if (!IsValidFileHeader(&file_header)) {
    log("invalid sparse image header: ");
    DumpStruct(log, &file_header, sizeof(file_header));
    return false;
  }

  uint64_t in_offset = file_header.file_hdr_sz;
  uint64_t out_offset = 0;
  for (size_t i = 0; i < LE32(file_header.total_chunks); i++) {
    if (in_offset + sizeof(chunk_header_t) > size) {
      log("Chunk header exceeds handle length\n");
      return false;
    }
    chunk_header_t chunk;
    if (!io->handle_ops.read(src, in_offset, (uint8_t*)(&chunk), sizeof(chunk_header_t))) {
      log("Failed to read chunk header\n");
      return false;
    }

    in_offset += LE16(file_header.chunk_hdr_sz);

    switch (LE16(chunk.chunk_type)) {
      case CHUNK_TYPE_RAW:
        if (!ValidateChunkSize(&file_header, &chunk,
                               (size_t)LE32(file_header.blk_sz) * LE32(chunk.chunk_sz))) {
          log("inconsistent chunk size for chunk: ");
          DumpStruct(log, &chunk, sizeof(chunk));
          return false;
        }

        size_t data_len = (size_t)LE32(chunk.chunk_sz) * LE32(file_header.blk_sz);
        if (in_offset + data_len > size) {
          log("chunk exceeds handle length\n");
          return false;
        }
        if (!io->write(io->ctx, out_offset, src, in_offset, data_len)) {
          log("Failed to write %lu @ %lu from %lu\n", data_len, out_offset, in_offset);
          return false;
        }
        in_offset += data_len;
        out_offset += data_len;
        break;
      case CHUNK_TYPE_DONT_CARE:
        if (!ValidateChunkSize(&file_header, &chunk, 0)) {
          log("inconsistent chunk size for chunk: ");
          DumpStruct(log, &chunk, sizeof(chunk));
          return false;
        }
        out_offset += (size_t)LE32(chunk.chunk_sz) * LE32(file_header.blk_sz);
        break;
      case CHUNK_TYPE_FILL:
        if (!ValidateChunkSize(&file_header, &chunk, sizeof(uint32_t))) {
          log("inconsistent chunk size for chunk: ");
          DumpStruct(log, &chunk, sizeof(chunk));
          return false;
        }

        uint32_t fill;
        if (in_offset + sizeof(fill) > size) {
          log("chunk exceeds handle length\n");
          return false;
        }
        if (!io->handle_ops.read(src, in_offset, (uint8_t*)&fill, sizeof(fill))) {
          log("Failed to read fill word\n");
          return false;
        }
        fill = LE32(fill);
        in_offset += sizeof(fill);

        if (first_fill || fill != fill_data[0]) {
          // Minor optimization if there are consecutive fill chunks with the same payload.
          first_fill = false;
          for (size_t j = 0; j < sizeof(fill_data) / sizeof(fill_data[0]); ++j) {
            fill_data[j] = fill;
          }
          if (!io->handle_ops.write(io->scratch_handle, 0, (uint8_t*)fill_data,
                                    sizeof(fill_data))) {
            log("Failed to fill scratch handle\n");
            return false;
          }
        }

        for (size_t j = 0; j < LE32(chunk.chunk_sz); j++) {
          if (!io->write(io->ctx, out_offset, io->scratch_handle, 0, LE32(file_header.blk_sz))) {
            log("Failed to write from scratch buffer\n");
            return false;
          }
          out_offset += LE32(file_header.blk_sz);
        }
        break;
      case CHUNK_TYPE_CRC32:
        if (!ValidateChunkSize(&file_header, &chunk, sizeof(uint32_t))) {
          log("inconsistent chunk size for chunk: ");
          DumpStruct(log, &chunk, sizeof(chunk));
          return false;
        }
        // Ignore CRC32 chunks for the time being
        in_offset += sizeof(uint32_t);
        break;
      default:
        log("unexpected chunk type: 0x%08X\n", LE16(chunk.chunk_type));
        return false;
    }
  }

  return true;
}
