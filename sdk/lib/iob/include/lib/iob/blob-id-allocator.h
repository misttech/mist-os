// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/result.h>
#include <lib/stdcompat/atomic.h>
#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <atomic>
#include <cstdint>
#include <limits>

#ifndef LIB_IOB_BLOB_ID_ALLOCATOR_H_
#define LIB_IOB_BLOB_ID_ALLOCATOR_H_

namespace iob {

// Represents a thread-safe view into an IOBuffer region of "ID allocator"
// discipline, used to map sized data blobs to sequentially-allocated numeric
// IDs.
//
// Suppose there are N mapped blobs. The memory is laid out as follows, with
// copies of the blobs growing down and their corresponding bookkeeping indices
// growing up:
// --------------------------------
//   next available blob ID (4 bytes)
//   blob head offset (4 bytes)
//   ----------------------------
//   blob 0 size (4 bytes)       } <-- bookkeeping index
//   blob 0 offset (4 bytes)     }
//   ...
//   blob N-1 size (4 bytes)
//   blob N-1 offset (4 bytes)
//   ----------------------------
//   zero-initialized memory      <-- remaining bytes available
//   ---------------------------- <-- blob head offset
//   blob N-1
//   ...
//   blob 0
// --------------------------------
//
// This class takes care of the atomic nuance required of accessing and updating
// such a structure.
//
// TODO(https://fxbug.dev/319501447): Reference the corresponding
// ZX_IOB_DISCIPLINE_TYPE_* value once defined.
class BlobIdAllocator {
 public:
  // Constructs a view into an ID allocator IOBuffer region. If the provided
  // memory does not yet reflect this layout, Init() must be called before any
  // other method.
  //
  // The provided memory must be at least 8-byte-aligned (for aligned atomic
  // access), and at least 8 bytes in size (so as to have enough room for the
  // header).
  explicit BlobIdAllocator(cpp20::span<std::byte> bytes) : bytes_(bytes) {
    ZX_ASSERT(sizeof(Header) <= bytes_.size());
    ZX_ASSERT(bytes_.size() <= std::numeric_limits<uint32_t>::max());
  }

  // Initializes the backing memory as an ID allocator region with no blobs yet
  // mapped. If the region is already known to be zero-filled, this part may be
  // skipped.
  void Init(bool zero_fill = true) {
    // Be sure to zero the region first and then store the header with release
    // semantics, so that any reader of the header will be guaranteed to see a
    // fully initialized structure.
    if (zero_fill) {
      memset(&bytes_[sizeof(Header)], 0, bytes_.size() - sizeof(Header));
    }
    header().store(
        Header{
            .next_id = 0,
            .blob_head = static_cast<uint32_t>(bytes_.size()),
        },
        std::memory_order_release);
  }

  // The remaining number of available bytes in the allocator (including those
  // that might be used for bookkeeping). fit::failed() is returned in the case
  // of an invalid header (see `AllocateError::kInvalidHeader` for more
  // detail).
  fit::result<fit::failed, size_t> RemainingBytes() const {
    return header().load(std::memory_order_relaxed).RemainingBytes(bytes_.size());
  }

  // The possible failure modes of Allocate().
  enum class AllocateError {
    // The header was invalid (purportedly either with indices extending past
    // the blob head offset or the blob head offset extending past the length
    // of the buffer). Suggests invalid initialization or corruption.
    kInvalidHeader,

    // The would-be bookkeeping index slot for the new allocated ID is
    // non-empty (i.e., not in the initialized state). Suggests corruption.
    kNonEmptyIndex,

    // Out Of Memory: there is insufficient memory available for the requested
    // allocation. (What is available can be queried with RemainingBytes().)
    kOutOfMemory,
  };

  // Attempts to store the provided blob and allocate its ID.
  fit::result<AllocateError, uint32_t> Allocate(cpp20::span<const std::byte> blob) {
    cpp20::atomic_ref<Header> ref = header();
    Header hdr = ref.load(std::memory_order_relaxed);
    bool retry = true;
    while (retry) {
      size_t remaining;
      if (auto result = hdr.RemainingBytes(bytes_.size()); result.is_error()) {
        return fit::error{AllocateError::kInvalidHeader};
      } else {
        remaining = result.value();
      }

      if (remaining < sizeof(Index) || remaining - sizeof(Index) < blob.size()) {
        return fit::error{AllocateError::kOutOfMemory};
      }
      Header updated = {
          .next_id = hdr.next_id + 1,
          .blob_head = hdr.blob_head - static_cast<uint32_t>(blob.size()),
      };
      retry = !ref.compare_exchange_weak(hdr, updated, std::memory_order_relaxed);
    }

    // When the loop terminates `hdr` reflects the header just prior to the
    // update.
    auto [id, offset] = hdr;
    offset -= static_cast<uint32_t>(blob.size());

    // We store the blob and then store the index with release semantics to
    // ensure the following:
    // (1) The write of the index stays ordered after the previous store of the
    //     blob.
    // (2) The write of the index stays ordered before subsequent reads, which
    //     are made with release semantics.
    memcpy(&bytes_[offset], blob.data(), blob.size());

    // Before overwriting, to be safe, check that the index is in the initial
    // state (i.e., empty).
    Index empty = {0, 0};
    Index index{.size = static_cast<uint32_t>(blob.size()), .offset = offset};
    if (!GetIndex(id).compare_exchange_strong(empty,                         //
                                              index,                         //
                                              std::memory_order_release)) {  //
      return fit::error{AllocateError::kNonEmptyIndex};
    }
    return fit::ok(id);
  }

  // The possible failure modes of GetBlob().
  enum class GetBlobError {
    // The header was invalid (purportedly either with indices extending past
    // the blob head offset or the blob head offset extending past the length
    // of the buffer). Suggests invalid initialization or corruption.
    kInvalidHeader,

    // The requested ID has not yet been allocated.
    kUnallocatedId,

    // Suggests a lost race in which the requested ID is valid and the header
    // has been updated to reflect that, but the bookkeeping index has not yet
    // been committed. Such a case unfortunately is indistinguishable from the
    // other possibility that the index was already committed but then
    // subsequently corrupted with zeroes. Where there is confidence in the
    // first case, this call should be retried.
    kUncommittedIndex,

    // The corresponding bookkeeping index is invalid, with the blob
    // purportedly not being contained within [blob head, end of region).
    // Suggests corruption.
    kInvalidIndex,
  };

  // Returns the blob corresponding to a given ID.
  fit::result<GetBlobError, cpp20::span<const std::byte>> GetBlob(uint32_t id) const {
    Header hdr = header().load(std::memory_order_relaxed);
    if (!hdr.IsValid(bytes_.size())) [[unlikely]] {
      return fit::error{GetBlobError::kInvalidHeader};
    }
    if (id >= hdr.next_id) {
      return fit::error{GetBlobError::kUnallocatedId};
    }

    // We load the index with acquire semantics as this ensures the following:
    // (1) The read of the index stays ordered before the subsequent load of the
    //     blob.
    // (2) The read of the index stays ordered after previous updates, which
    //     were written with release semantics.
    Index index = GetIndex(id).load(std::memory_order_acquire);
    if (index.size == 0 && index.offset == 0) [[unlikely]] {
      return fit::error{GetBlobError::kUncommittedIndex};
    }
    if (!index.IsValid(hdr.blob_head, bytes_.size())) [[unlikely]] {
      return fit::error{GetBlobError::kInvalidIndex};
    }
    return fit::ok(bytes_.subspan(index.offset, index.size));
  }

 private:
  struct alignas(8) Header {
    constexpr uint32_t index_end() const { return sizeof(Header) + next_id * sizeof(Index); }

    // See AllocateError::kInvalidHeader.
    constexpr bool IsValid(size_t length) const {
      return index_end() <= blob_head && blob_head <= length;
    }

    // Returns the remaining number of available bytes in the region, given the
    // length of the region - or fit::failed() in the event of an invalid
    // header.
    constexpr fit::result<fit::failed, size_t> RemainingBytes(size_t length) const {
      if (!IsValid(length)) [[unlikely]] {
        return fit::failed();
      }
      return fit::ok(blob_head - index_end());
    }

    uint32_t next_id;
    uint32_t blob_head;
  };

  // Represents a blob bookkeeping index.
  struct alignas(8) Index {
    // See GetBlobError::kInvalidIndex. The current blob head offset and length
    // of the region must be provided, and are assumed to have already been
    // validated.
    constexpr bool IsValid(uint32_t blob_head, size_t length) const {
      ZX_DEBUG_ASSERT(blob_head <= length);
      return blob_head <= offset && offset <= length && size <= length - offset;
    }

    uint32_t size;
    uint32_t offset;
  };

  static_assert(cpp20::atomic_ref<Header>::is_always_lock_free);
  cpp20::atomic_ref<Header> header() {
    return cpp20::atomic_ref{*reinterpret_cast<Header*>(bytes_.data())};
  }

  cpp20::atomic_ref<const Header> header() const {
    return cpp20::atomic_ref{*reinterpret_cast<const Header*>(bytes_.data())};
  }

  static_assert(cpp20::atomic_ref<Index>::is_always_lock_free);
  cpp20::atomic_ref<Index> GetIndex(uint32_t id) {
    return cpp20::atomic_ref{*reinterpret_cast<Index*>(&bytes_[(id + 1) * sizeof(Index)])};
  }
  cpp20::atomic_ref<const Index> GetIndex(uint32_t id) const {
    return cpp20::atomic_ref{*reinterpret_cast<const Index*>(&bytes_[(id + 1) * sizeof(Index)])};
  }

  cpp20::span<std::byte> bytes_;
};

}  // namespace iob

#endif  // LIB_IOB_BLOB_ID_ALLOCATOR_H_
