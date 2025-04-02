// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_SPSC_BUFFER_INCLUDE_LIB_SPSC_BUFFER_SPSC_BUFFER_H_
#define ZIRCON_KERNEL_LIB_SPSC_BUFFER_INCLUDE_LIB_SPSC_BUFFER_SPSC_BUFFER_H_

#include <lib/zx/result.h>
#include <zircon/types.h>

#include <ktl/algorithm.h>
#include <ktl/atomic.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/span.h>

// SpscBuffer implements a transactional, single-producer, single-consumer ring buffer.
//
// The caller is responsible for ensuring that there is only one reader and one writer; no internal
// synchronization is provided to enforce this constraint.
//
// The caller is also responsible for providing the underlying memory region in which to store
// data. A span pointing to this buffer must be provided to the Init method before Read/Reserve can
// be called. Note that this backing buffer must have a size that is a power of two for correct
// functionality.
class SpscBuffer {
 public:
  SpscBuffer() = default;
  ~SpscBuffer() = default;

  // An SpscBuffer should be initialized once and never moved or copied.
  SpscBuffer(const SpscBuffer&) = delete;
  SpscBuffer& operator=(const SpscBuffer&) = delete;
  SpscBuffer(SpscBuffer&&) = delete;
  SpscBuffer operator=(SpscBuffer&&) = delete;

  // Initializes the buffer using the provided buffer as backing storage.
  void Init(ktl::span<ktl::byte> buffer) {
    // Assert that the provided buffer meets our requirements for the storage buffer.
    // See the comments around storage_ for more information about these requirements.
    DEBUG_ASSERT(buffer.size() <= (1u << 31));
    DEBUG_ASSERT((buffer.size() & (buffer.size() - 1)) == 0);
    storage_ = buffer;
  }

  // A simple convenience type used to hold the read and write pointers as separate values.
  struct RingPointers {
    uint32_t read;
    uint32_t write;
  };

  zx_status_t Reserve(uint32_t size) {
    // TODO(https://fxbug.dev/404539312): Implement reservations.
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t Read(void* dst, uint32_t offset, uint32_t len) {
    // TODO(https://fxbug.dev/404539312): Implement reads.
    return ZX_ERR_NOT_SUPPORTED;
  }

 private:
  friend class SpscBufferTests;

  // Helper function that converts read and write pointers into ring buffer offsets.
  uint32_t PointerToOffset(const uint32_t pointer) const {
    // This is logically performing pointer % storage_.size(), but because storage_.size() is
    // guaranteed to be a power of two it is equivalent to this logical AND.
    return pointer & (static_cast<uint32_t>(storage_.size()) - 1);
  }

  static RingPointers SplitCombinedPointers(const uint64_t combined) {
    return {
        .read = static_cast<uint32_t>(combined >> 32),
        .write = static_cast<uint32_t>(combined),
    };
  }
  static uint64_t CombinePointers(const RingPointers& pointers) {
    uint64_t read_64 = static_cast<uint64_t>(pointers.read);
    return (read_64 << 32) | pointers.write;
  }

  // The amount of data available to read is just the write pointer minus the read pointer.
  // This works even if the write pointer has overflowed and wrapped back around to zero
  // because unsigned subtraction in C++ uses modular arithmetic.
  static uint32_t AvailableData(const RingPointers& pointers) {
    return pointers.write - pointers.read;
  }
  uint32_t AvailableSpace(const RingPointers& pointers) const {
    return static_cast<uint32_t>(storage_.size()) - AvailableData(pointers);
  }

  // Helper function that loads the current values of the read and write pointers.
  //
  // This is a load-acquire operation that synchronizes with the store-release in the
  // Advance[Read|Write]Pointer functions. By using acquire semantics, we ensure that any stores
  // that occur prior to an Advance[Read|Write]Pointer store-release operation that we synchronize
  // with are observable by this thread after the load.
  RingPointers LoadPointers() const {
    const uint64_t combined = combined_pointers_.load(ktl::memory_order_acquire);
    return SplitCombinedPointers(combined);
  }

  // The AdvanceReadPointer and AdvanceWritePointer functions add the given delta to either the read
  // or write half of the combined_pointers_.
  //
  // Because we store the pointers in a single combined atomic variable, we must update the entire
  // combined pointer. We perform this update using a compare and exchange to ensure that concurrent
  // operations to the other half of the combined pointers are preserved. We also check to ensure
  // reads do not encounter concurrent reads, and writes do not encounter concurrent writes.
  //
  // This is a store-release operation that synchronizes with the load-acquire in LoadPointer.
  // By using release semantics, we ensure that if the updated value is seen in LoadPointers,
  // all memory operations that occurred prior to this update are observable.
  void AdvanceReadPointer(const RingPointers& initial, uint32_t delta) {
    // We should never be advancing the read pointer by more than the amount of data available in
    // the buffer.
    DEBUG_ASSERT(delta <= AvailableData(initial));

    const uint32_t target_read = initial.read + delta;

    uint64_t starting_pointers = CombinePointers(initial);
    uint64_t target_pointers = CombinePointers({.read = target_read, .write = initial.write});

    while (!combined_pointers_.compare_exchange_weak(
        starting_pointers, target_pointers, ktl::memory_order_release, ktl::memory_order_relaxed)) {
      const RingPointers observed = SplitCombinedPointers(starting_pointers);
      DEBUG_ASSERT_MSG(observed.read == initial.read,
                       "potential concurrent read detected; expected read pointer %u, got %u",
                       initial.read, observed.read);
      target_pointers = CombinePointers({.read = target_read, .write = observed.write});
    }
  }
  void AdvanceWritePointer(const RingPointers& initial, uint32_t delta) {
    // We should never be advancing the write pointer by more than the amount of empty space in the
    // buffer.
    DEBUG_ASSERT(delta <= AvailableSpace(initial));

    const uint32_t target_write = initial.write + delta;

    uint64_t starting_pointers = CombinePointers(initial);
    uint64_t target_pointers = CombinePointers({.read = initial.read, .write = target_write});

    while (!combined_pointers_.compare_exchange_weak(
        starting_pointers, target_pointers, ktl::memory_order_release, ktl::memory_order_relaxed)) {
      const RingPointers observed = SplitCombinedPointers(starting_pointers);
      DEBUG_ASSERT_MSG(observed.write == initial.write,
                       "potential concurrent write detected; expected write pointer %u, got %u",
                       initial.write, observed.write);
      target_pointers = CombinePointers({.read = observed.read, .write = target_write});
    }
  }

  // The read and write pointer are stored as the upper and lower halves, respectively, of a single
  // 64-bit atomic.
  //
  // The read and write pointers:
  // * Are unsigned 32-bit integers that monotonically increase as reads and writes are performed.
  // * Wrap when they reach UINT32_MAX, not when they reach the size of storage_. The offset into
  //   storage_ remains correct because it is required to have a size that is a power of two.
  // * Can be at most storage_.size() bytes apart.
  ktl::atomic<uint64_t> combined_pointers_{0};

  // The underlying storage used to store the data.
  //
  // This buffer MUST:
  // * Have a size that is a power of two, otherwise the modular arithmetic used to translate
  //   between pointers and offsets does not work correctly when a pointer overflows.
  // * Have a size smaller than 2^32 bytes because the read and write pointer must fit into 32
  //   bits. Taken in conjunction with the previous requirement, this means that the buffer must
  //   have a size less than or equal to 2^31 bytes.
  ktl::span<ktl::byte> storage_;
};

#endif  // ZIRCON_KERNEL_LIB_SPSC_BUFFER_INCLUDE_LIB_SPSC_BUFFER_SPSC_BUFFER_H_
