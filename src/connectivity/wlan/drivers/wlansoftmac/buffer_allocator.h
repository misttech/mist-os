// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_BUFFER_ALLOCATOR_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_BUFFER_ALLOCATOR_H_

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <memory>

#include <fbl/slab_allocator.h>
#include <wlan/common/logging.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers {

// A Buffer is a type that points at bytes and knows how big it is.
class Buffer {
 public:
  virtual ~Buffer() = default;
  virtual uint8_t* data() = 0;
  virtual size_t capacity() const = 0;
  virtual void clear(size_t len) = 0;
  enum Size : uint8_t {
    kSmall,
    kLarge,
    kHuge,
  };
};

class FinalizedBuffer {
 public:
  FinalizedBuffer(FinalizedBuffer&&) noexcept = default;
  FinalizedBuffer& operator=(FinalizedBuffer&&) noexcept = default;

  FinalizedBuffer() = delete;
  FinalizedBuffer(const FinalizedBuffer&) noexcept = delete;
  FinalizedBuffer& operator=(const FinalizedBuffer&) noexcept = delete;

  FinalizedBuffer(Buffer* buffer, size_t written)
      : buffer_(std::unique_ptr<Buffer>(buffer)), written_(written) {}

  const uint8_t* data() const { return buffer_->data(); }
  size_t written() const { return written_; }

 private:
  std::unique_ptr<Buffer> buffer_;
  size_t written_;
};

// Huge buffers are used for sending lots of data between drivers and the
// wlanstack.
constexpr size_t kHugeSlabs = 2;
constexpr size_t kHugeBuffers = 8;
constexpr size_t kHugeBufferSize = 16384;
// Large buffers can hold the largest 802.11 MSDU, standard Ethernet MTU,
// or HT A-MSDU of size 3,839 bytes.
constexpr size_t kLargeSlabs = 20;
constexpr size_t kLargeBuffers = 32;
constexpr size_t kLargeBufferSize = 4096;
// Small buffers are for smaller control packets within the driver stack itself
// and for transferring small 802.11 frames as well.
constexpr size_t kSmallSlabs = 40;
constexpr size_t kSmallBuffers = 512;
constexpr size_t kSmallBufferSize = 256;

// TODO(eyw): Revisit SlabAllocator counter behavior in Zircon to remove the
// dependency on template
template <typename, typename, typename, bool>
class BufferDebugger;

template <typename SmallSlabAllocator, typename LargeSlabAllocator, typename HugeSlabAllocator>
class BufferDebugger<SmallSlabAllocator, LargeSlabAllocator, HugeSlabAllocator, true> {
 public:
  static void Fail(Buffer::Size size) {
    // TODO(eyw): Use a timer to throttle logging
    switch (size) {
      case Buffer::Size::kSmall:
        if (is_exhausted_small_) {
          return;
        }
        is_exhausted_small_ = true;
        debugbuf("Small buffer exhausted.");
        break;
      case Buffer::Size::kLarge:
        if (is_exhausted_large_) {
          return;
        }
        is_exhausted_large_ = true;
        debugbuf("Large buffer exhausted.");
        break;
      case Buffer::Size::kHuge:
        if (is_exhausted_huge_) {
          return;
        }
        is_exhausted_huge_ = true;
        debugbuf("Huge buffer exhausted.");
        break;
    }
    PrintCounters();
  }
  static void PrintCounters() {
    // 4 numbers for each allocator:
    // current buffers in use / historical maximum buffers in use /
    // current allocator capacity / maximum allocator capacity
    debugbuf(
        "usage(in_use/in_use_max/current_capacity/max_capacity)\n Small: "
        "%zu/%zu/%zu/%zu, "
        "Large: %zu/%zu/%zu/%zu, Huge: %zu/%zu/%zu/%zu\n",
        SmallSlabAllocator::obj_count(), SmallSlabAllocator::max_obj_count(),
        SmallSlabAllocator::slab_count() * kSmallBuffers, kSmallSlabs * kSmallBuffers,
        LargeSlabAllocator::obj_count(), LargeSlabAllocator::max_obj_count(),
        LargeSlabAllocator::slab_count() * kLargeBuffers, kLargeSlabs * kLargeBuffers,
        HugeSlabAllocator::obj_count(), HugeSlabAllocator::max_obj_count(),
        HugeSlabAllocator::slab_count() * kHugeBuffers, kHugeSlabs * kHugeBuffers);
  }

 private:
  static bool is_exhausted_small_;
  static bool is_exhausted_large_;
  static bool is_exhausted_huge_;
};

template <typename SmallSlabAllocator, typename LargeSlabAllocator, typename HugeSlabAllocator>
bool BufferDebugger<SmallSlabAllocator, LargeSlabAllocator, HugeSlabAllocator,
                    true>::is_exhausted_small_ = false;

template <typename SmallSlabAllocator, typename LargeSlabAllocator, typename HugeSlabAllocator>
bool BufferDebugger<SmallSlabAllocator, LargeSlabAllocator, HugeSlabAllocator,
                    true>::is_exhausted_large_ = false;

template <typename SmallSlabAllocator, typename LargeSlabAllocator, typename HugeSlabAllocator>
bool BufferDebugger<SmallSlabAllocator, LargeSlabAllocator, HugeSlabAllocator,
                    true>::is_exhausted_huge_ = false;

template <typename SmallSlabAllocator, typename LargeSlabAllocator, typename HugeSlabAllocator>
class BufferDebugger<SmallSlabAllocator, LargeSlabAllocator, HugeSlabAllocator, false> {
 public:
  static void Fail(...) {}
  static void PrintCounters() {}
};

namespace internal {
template <size_t BufferSize>
class FixedBuffer : public Buffer {
 public:
  uint8_t* data() override { return data_; }
  size_t capacity() const override { return BufferSize; }
  void clear(size_t len) override { std::memset(data_, 0, std::min(BufferSize, len)); }

 private:
  uint8_t data_[BufferSize];
};
}  // namespace internal

constexpr size_t kSlabOverhead = 16;  // overhead for the slab allocator as a whole

template <size_t NumBuffers, size_t BufferSize>
class SlabBuffer;
template <size_t NumBuffers, size_t BufferSize>
using SlabBufferTraits = fbl::StaticSlabAllocatorTraits<
    std::unique_ptr<SlabBuffer<NumBuffers, BufferSize>>,
    sizeof(internal::FixedBuffer<BufferSize>) * NumBuffers + kSlabOverhead, ::fbl::Mutex,
    kBufferDebugEnabled ? fbl::SlabAllocatorOptions::EnableObjectCount
                        : fbl::SlabAllocatorOptions::None>;

// A SlabBuffer is an implementation of a Buffer that comes from a
// fbl::SlabAllocator. The size of the internal::FixedBuffer and the number of
// buffers is part of the typename of the SlabAllocator, so the SlabBuffer
// itself is also templated on these parameters.
template <size_t NumBuffers, size_t BufferSize>
class SlabBuffer final : public internal::FixedBuffer<BufferSize>,
                         public fbl::SlabAllocated<SlabBufferTraits<NumBuffers, BufferSize>> {};

using HugeBufferTraits = SlabBufferTraits<kHugeBuffers, kHugeBufferSize>;
using LargeBufferTraits = SlabBufferTraits<kLargeBuffers, kLargeBufferSize>;
using SmallBufferTraits = SlabBufferTraits<kSmallBuffers, kSmallBufferSize>;
using HugeBufferAllocator = fbl::SlabAllocator<HugeBufferTraits>;
using LargeBufferAllocator = fbl::SlabAllocator<LargeBufferTraits>;
using SmallBufferAllocator = fbl::SlabAllocator<SmallBufferTraits>;

// Gets a (slab allocated) Buffer with at least |len| bytes capacity.
std::unique_ptr<Buffer> GetBuffer(size_t len);

}  // namespace wlan::drivers

// Declaration of static slab allocators.
FWD_DECL_STATIC_SLAB_ALLOCATOR(::wlan::drivers::HugeBufferTraits);
FWD_DECL_STATIC_SLAB_ALLOCATOR(::wlan::drivers::LargeBufferTraits);
FWD_DECL_STATIC_SLAB_ALLOCATOR(::wlan::drivers::SmallBufferTraits);

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_BUFFER_ALLOCATOR_H_
