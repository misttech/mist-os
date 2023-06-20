// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/storage/gpt_utils.h>
#include <lib/storage/sparse.h>
#include <sparse_format.h>
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

static void DumpStruct(void* s, size_t size) {
#ifdef ENABLE_FIRMWARE_STORAGE_LOG
  for (size_t i = 0; i < size; i++) {
    firmware_storage_log("0x%02X ", ((uint8_t*)s)[i]);
  }
  firmware_storage_log("\n");
#endif
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
// @src: Buffer containing a sparse image.
// @size: Maximum size of the sparse image buffer.
// @header: Output parameter for the validated sparse header. Must be properly aligned.
//
// Returns true if the header is valid, false otherwise.
// The header output parameter is only valid if the function returns true.
static bool ValidateHeader(const uint8_t* src, size_t size, sparse_header_t* header) {
  if (!src || size < sizeof(*header)) {
    return false;
  }
  memcpy(header, src, sizeof(*header));
  return IsValidFileHeader(header);
}

// Check that a buffer points to memory that could be a valid chunk header
// and if it is valid copy the chunk header to an aligned chunk header struct.
//
// @src: Pointer to a chunk in a larger buffer.
// @end: The end of the buffer.
// @chunk_hdr_sz: The size of a chunk header, taken from the file header
// @chunk: Output parameter for the validated chunk header. Must be properly aligned.
//
// Returns true if the chunk header is valid, false otherwise.
// The chunk output parameter is only valid if the function returns true.
static bool ValidateChunk(const uint8_t* src, const uint8_t* end, size_t chunk_hdr_size,
                          chunk_header_t* chunk) {
  if (!src || chunk_hdr_size < sizeof(*chunk)) {
    return false;
  }
  memcpy(chunk, src, sizeof(*chunk));
  return src + chunk_hdr_size <= end;
}

static bool ValidateChunkSize(const sparse_header_t* header, const chunk_header_t* chunk,
                              size_t payload_size) {
  return LE32(chunk->total_sz) == header->chunk_hdr_sz + payload_size;
}

bool FuchsiaIsSparseImage(const uint8_t* src, size_t size) {
  sparse_header_t header;
  return ValidateHeader(src, size, &header);
}

bool FuchsiaWriteSparseImage(FuchsiaFirmwareStorage* ops, const GptData* gpt_data, const char* name,
                             uint8_t* src, size_t size) {
  static uint32_t fill_data[kFillInts] = {};

  sparse_header_t file_header;
  if (!ValidateHeader(src, size, &file_header)) {
    firmware_storage_log("invalid sparse image header: ");
    DumpStruct(src, sizeof(file_header));
    return false;
  }

  size_t part_offset = 0;
  uint8_t* chunk_ptr = src + LE16(file_header.file_hdr_sz);
  for (size_t i = 0; i < LE32(file_header.total_chunks); i++) {
    chunk_header_t chunk;
    if (!ValidateChunk(chunk_ptr, src + size, LE16(file_header.chunk_hdr_sz), &chunk)) {
      firmware_storage_log("invalid chunk header: ");
      DumpStruct(chunk_ptr, LE16(file_header.chunk_hdr_sz));
      return false;
    }

    switch (LE16(chunk.chunk_type)) {
      case CHUNK_TYPE_RAW:
        if (!ValidateChunkSize(&file_header, &chunk,
                               (size_t)LE32(file_header.blk_sz) * LE32(chunk.chunk_sz))) {
          firmware_storage_log("inconsistent chunk size for chunk: ");
          DumpStruct(chunk_ptr, LE16(file_header.chunk_hdr_sz));
          return false;
        }

        uint8_t* raw_data = chunk_ptr + LE16(file_header.chunk_hdr_sz);
        size_t raw_len = (size_t)LE32(chunk.chunk_sz) * LE32(file_header.blk_sz);
        if (!FuchsiaFirmwareStorageGptWrite(ops, gpt_data, name, part_offset, raw_len, raw_data)) {
          firmware_storage_log("write failure to partition: %s\n", name);
          return false;
        }

        part_offset += raw_len;
        break;
      case CHUNK_TYPE_DONT_CARE:
        if (!ValidateChunkSize(&file_header, &chunk, 0)) {
          firmware_storage_log("inconsistent chunk size for chunk: ");
          DumpStruct(chunk_ptr, LE16(file_header.chunk_hdr_sz));
          return false;
        }

        part_offset += (size_t)LE32(chunk.chunk_sz) * LE32(file_header.blk_sz);
        break;
      case CHUNK_TYPE_FILL:
        if (!ValidateChunkSize(&file_header, &chunk, sizeof(uint32_t))) {
          firmware_storage_log("inconsistent chunk size for chunk: ");
          DumpStruct(chunk_ptr, LE16(file_header.chunk_hdr_sz));
          return false;
        }

        uint32_t data;
        memcpy(&data, chunk_ptr + LE16(file_header.chunk_hdr_sz), sizeof(data));
        data = LE32(data);
        // Minor optimization if there are consecutive fill chunks with the same payload.
        if (data != fill_data[0]) {
          for (size_t j = 0; j < kFillInts; j++) {
            fill_data[j] = data;
          }
        }

        for (size_t j = 0; j < LE32(chunk.chunk_sz); j++) {
          if (!FuchsiaFirmwareStorageGptWrite(ops, gpt_data, name, part_offset,
                                              LE32(file_header.blk_sz), fill_data)) {
            firmware_storage_log("write failure to partition: %s\n", name);
            return false;
          }
          part_offset += LE32(file_header.blk_sz);
        }
        break;
      case CHUNK_TYPE_CRC32:
        if (!ValidateChunkSize(&file_header, &chunk, sizeof(uint32_t))) {
          firmware_storage_log("inconsistent chunk size for chunk: ");
          DumpStruct(chunk_ptr, LE16(file_header.chunk_hdr_sz));
          return false;
        }
        // Ignore CRC32 chunks for the time being
        break;
      default:
        firmware_storage_log("unexpected chunk type: 0x%08X\n", LE16(chunk.chunk_type));
        return false;
    }

    chunk_ptr += LE32(chunk.total_sz);
  }

  return true;
}
