// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/zx/fifo.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <zxtest/zxtest.h>

namespace {

using ElementType = uint64_t;
constexpr size_t kElementSize = sizeof(ElementType);

zx_signals_t GetSignals(const zx::fifo& fifo) {
  zx_signals_t pending;
  zx_status_t status = fifo.wait_one(0xFFFFFFFF, zx::time(), &pending);
  if ((status != ZX_OK) && (status != ZX_ERR_TIMED_OUT)) {
    return 0xFFFFFFFF;
  }
  return pending;
}

#define EXPECT_SIGNALS(h, s) EXPECT_EQ(GetSignals(h), s)

// Helper class to map a VMO somewhere in the address space with a page of padding to either side.
// Cleans up on destruction.
class VmoMapWithPadding {
 public:
  VmoMapWithPadding() = default;
  ~VmoMapWithPadding() {
    if (vmar_.is_valid()) {
      vmar_.unmap(vmar_addr_, vmar_size_);
      vmar_.destroy();
      vmar_.reset();
    }
  }

  zx_status_t Map(const zx::vmo& vmo, size_t vmo_size, zx_vaddr_t* out_addr) {
    if (vmar_.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }

    // Create a vmar with a page on either side as padding.
    vmar_size_ = vmo_size + 2 * zx_system_get_page_size();
    zx_status_t err = zx::vmar::root_self()->allocate(
        ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_SPECIFIC, 0, vmar_size_, &vmar_,
        &vmar_addr_);
    if (err != ZX_OK) {
      return err;
    }

    // Map the passed in vmo.
    zx_vaddr_t addr;
    err = vmar_.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC, zx_system_get_page_size(),
                    vmo, 0, vmo_size, &addr);
    if (err != ZX_OK) {
      return err;
    }

    *out_addr = addr;
    return ZX_OK;
  }

 private:
  zx::vmar vmar_;
  zx_vaddr_t vmar_addr_ = 0;
  size_t vmar_size_ = 0;
};

TEST(FifoTest, InvalidParametersReturnOutOfRange) {
  zx::fifo fifo_a, fifo_b;

  // ensure parameter validation works
  EXPECT_EQ(zx::fifo::create(0, 0, 0, &fifo_a, &fifo_b),
            ZX_ERR_OUT_OF_RANGE);  // too small
  EXPECT_EQ(zx::fifo::create(128, 33, 0, &fifo_a, &fifo_b),
            ZX_ERR_OUT_OF_RANGE);  // too large
  EXPECT_EQ(zx::fifo::create(0, 0, 1, &fifo_a, &fifo_b),
            ZX_ERR_OUT_OF_RANGE);  // invalid options
}

TEST(FifoTest, EndpointsAreRelated) {
  zx::fifo fifo_a, fifo_b;

  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));
  EXPECT_SIGNALS(fifo_a, ZX_FIFO_WRITABLE);
  EXPECT_SIGNALS(fifo_b, ZX_FIFO_WRITABLE);

  // Check that koids line up.
  zx_info_handle_basic_t info_a = {}, info_b = {};
  ASSERT_OK(fifo_a.get_info(ZX_INFO_HANDLE_BASIC, &info_a, sizeof(info_a), nullptr, nullptr));

  ASSERT_OK(fifo_b.get_info(ZX_INFO_HANDLE_BASIC, &info_b, sizeof(info_b), nullptr, nullptr));
  ASSERT_NE(info_a.koid, 0u, "zero koid!");
  ASSERT_NE(info_a.related_koid, 0u, "zero peer koid!");
  ASSERT_NE(info_b.koid, 0u, "zero koid!");
  ASSERT_NE(info_b.related_koid, 0u, "zero peer koid!");
  ASSERT_EQ(info_a.koid, info_b.related_koid, "mismatched koids!");
  ASSERT_EQ(info_b.koid, info_a.related_koid, "mismatched koids!");
}

TEST(FifoTest, EmptyQueueReturnsErrShouldWait) {
  zx::fifo fifo_a, fifo_b;
  ElementType actual_elements[8] = {};

  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

  // should not be able to read any entries from an empty fifo
  size_t actual_count;
  EXPECT_EQ(fifo_a.read(kElementSize, actual_elements, 8, &actual_count), ZX_ERR_SHOULD_WAIT);
}

TEST(FifoTest, ReadAndWriteValidatesSizeAndElementCount) {
  zx::fifo fifo_a, fifo_b;
  ElementType expected_elements[] = {1, 2, 3, 4, 5, 6, 7, 8};
  ElementType actual_elements[8] = {};
  size_t actual_count;

  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

  // not allowed to read or write zero elements
  EXPECT_EQ(fifo_a.read(kElementSize, actual_elements, 0, &actual_count), ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(fifo_a.write(kElementSize, expected_elements, 0, &actual_count), ZX_ERR_OUT_OF_RANGE);

  // element size must match
  EXPECT_EQ(fifo_a.read(kElementSize + 1, actual_elements, 8, &actual_count), ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(fifo_a.write(kElementSize + 1, expected_elements, 8, &actual_count),
            ZX_ERR_OUT_OF_RANGE);
}

TEST(FifoTest, DequeueSignalsWriteable) {
  zx::fifo fifo_a, fifo_b;
  ElementType expected_elements[] = {1, 2, 3, 4, 5, 6, 7, 8};
  ElementType actual_elements[8] = {};
  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

  EXPECT_SIGNALS(fifo_a, ZX_FIFO_WRITABLE);
  EXPECT_SIGNALS(fifo_b, ZX_FIFO_WRITABLE);

  size_t actual_count;
  // should be able to write all entries into empty fifo
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 8, &actual_count));
  ASSERT_EQ(actual_count, 8u);
  EXPECT_SIGNALS(fifo_b, ZX_FIFO_READABLE | ZX_FIFO_WRITABLE);

  // should be able to write no entries into a full fifo
  ASSERT_EQ(fifo_a.write(kElementSize, expected_elements, 8, &actual_count), ZX_ERR_SHOULD_WAIT);
  EXPECT_SIGNALS(fifo_a, 0u);

  // read half the entries, make sure they're what we expect
  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, 4, &actual_count));
  ASSERT_EQ(actual_count, 4u);
  ASSERT_EQ(actual_elements[0], 1u);
  ASSERT_EQ(actual_elements[1], 2u);
  ASSERT_EQ(actual_elements[2], 3u);
  ASSERT_EQ(actual_elements[3], 4u);
  ASSERT_EQ(actual_elements[4], 0u);

  // should be writable again now
  EXPECT_SIGNALS(fifo_a, ZX_FIFO_WRITABLE);

  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, 4, &actual_count));
  ASSERT_EQ(actual_elements[0], 5u);
  ASSERT_EQ(actual_elements[1], 6u);
  ASSERT_EQ(actual_elements[2], 7u);
  ASSERT_EQ(actual_elements[3], 8u);
  ASSERT_EQ(actual_elements[4], 0u);

  // should no longer be readable
  EXPECT_SIGNALS(fifo_b, ZX_FIFO_WRITABLE);
}

TEST(FifoTest, FifoOrderIsPreserved) {
  zx::fifo fifo_a, fifo_b;
  ElementType expected_elements[] = {1, 2, 3, 4, 5, 6, 7, 8};
  ElementType actual_elements[8] = {};

  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;
  // should be able to write all entries into empty fifo
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 8, &actual_count));

  // read half the entries, make sure they're what we expect
  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, 4, &actual_count));

  // write some more, wrapping to the front again
  expected_elements[0] = 9u;
  expected_elements[1] = 10u;
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 2, &actual_count));
  ASSERT_EQ(actual_count, 2u);

  // read across the wrap, test partial read
  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, 8, &actual_count));
  ASSERT_EQ(actual_count, 6u);
  ASSERT_EQ(actual_elements[0], 5u);
  ASSERT_EQ(actual_elements[1], 6u);
  ASSERT_EQ(actual_elements[2], 7u);
  ASSERT_EQ(actual_elements[3], 8u);
  ASSERT_EQ(actual_elements[4], 9u);
  ASSERT_EQ(actual_elements[5], 10u);

  // write across the wrap
  expected_elements[0] = 11u;
  expected_elements[1] = 12u;
  expected_elements[2] = 13u;
  expected_elements[3] = 14u;
  expected_elements[4] = 15u;
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 5, &actual_count));
  ASSERT_EQ(actual_count, 5u);
}

TEST(FifoTest, PartialWriteQueuesElementsThatFit) {
  zx::fifo fifo_a, fifo_b;
  ElementType expected_elements[] = {1, 2, 3, 4, 5, 6, 7, 8};

  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;
  // Fill it up for 5 elements.
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 5, &actual_count));

  // partial write test
  expected_elements[0] = 16u;
  expected_elements[1] = 17u;
  expected_elements[2] = 18u;
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 5, &actual_count));
  ASSERT_EQ(actual_count, 3u);
}

TEST(FifoTest, IndividualReadsPreserveOrder) {
  zx::fifo fifo_a, fifo_b;
  ElementType expected_elements[] = {1, 2, 3, 4, 5, 6, 7, 8};

  // simple 8 x 8 fifo
  ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;
  // Fill it up
  ASSERT_OK(fifo_a.write(kElementSize, expected_elements, 8, &actual_count));

  ElementType actual_element;
  // small reads
  for (unsigned i = 0; i < 8; i++) {
    ASSERT_OK(fifo_b.read(kElementSize, &actual_element, 1, &actual_count));
    ASSERT_EQ(actual_count, 1u);
    ASSERT_EQ(actual_element, expected_elements[i]);
  }
}

TEST(FifoTest, EndpointCloseSignalsPeerClosed) {
  zx::fifo fifo_b;
  ElementType expected_element = 19u;
  ElementType actual_elements[8] = {};

  size_t actual_count;

  {
    zx::fifo fifo_a;
    ASSERT_OK(zx::fifo::create(8, kElementSize, 0, &fifo_a, &fifo_b));

    // write and then close, verify we can read written entries before
    // receiving ZX_ERR_PEER_CLOSED.
    ASSERT_OK(fifo_a.write(kElementSize, &expected_element, 1, &actual_count));
    ASSERT_EQ(actual_count, 1u);
    // end of scope for fifo_b so it's closed.
  }

  EXPECT_SIGNALS(fifo_b, ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED);
  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, 8, &actual_count));
  ASSERT_EQ(actual_count, 1u);
  EXPECT_SIGNALS(fifo_b, ZX_FIFO_PEER_CLOSED);
  ASSERT_EQ(fifo_b.read(kElementSize, actual_elements, 8, &actual_count), ZX_ERR_PEER_CLOSED);
  ASSERT_EQ(fifo_b.signal_peer(0u, ZX_USER_SIGNAL_0), ZX_ERR_PEER_CLOSED);
}

TEST(FifoTest, NonPowerOfTwoCountSupported) {
  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(10, kElementSize, 0, &fifo_a, &fifo_b));

  ElementType expected_elements[] = {1, 2, 3, 4, 5, 6, 7, 8, 9};
  ElementType actual_elements[9] = {};
  size_t actual_count;

  // Write to, then drain, the FIFO.
  // Intentionally write one element less than the FIFO can hold, so the next write will wrap.
  ASSERT_OK(
      fifo_a.write(kElementSize, &expected_elements, std::size(expected_elements), &actual_count));
  ASSERT_EQ(actual_count, 9u);
  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, std::size(actual_elements), &actual_count));
  ASSERT_EQ(actual_count, 9u);

  // Repeat the process. This write spans the buffer wrap.
  ASSERT_OK(
      fifo_a.write(kElementSize, &expected_elements, std::size(expected_elements), &actual_count));
  ASSERT_EQ(actual_count, 9u);
  ASSERT_OK(fifo_b.read(kElementSize, actual_elements, std::size(actual_elements), &actual_count));
  ASSERT_EQ(actual_count, 9u);
}

TEST(FifoTest, ReadNullBuffer) {
  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(4, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;

  ElementType element[] = {1};
  EXPECT_OK(fifo_a.write(kElementSize, &element, std::size(element), &actual_count));

  EXPECT_STATUS(fifo_b.read(kElementSize, nullptr, std::size(element), &actual_count),
                ZX_ERR_INVALID_ARGS);
}

TEST(FifoTest, WriteNullBuffer) {
  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(4, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;

  ElementType element[] = {1};
  EXPECT_STATUS(fifo_a.write(kElementSize, nullptr, std::size(element), &actual_count),
                ZX_ERR_INVALID_ARGS);
}

TEST(FifoTest, ReadBadBuffer) {
  const size_t kVmoSize = zx_system_get_page_size();
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kVmoSize, 0, &vmo));

  zx_vaddr_t addr;

  // Note, no options means the buffer is not readable.
  ASSERT_OK(zx::vmar::root_self()->map(0, 0, vmo, 0, kVmoSize, &addr));
  auto unmap = fit::defer([&]() { zx::vmar::root_self()->unmap(addr, kVmoSize); });

  void* buffer = reinterpret_cast<void*>(addr);

  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(4, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;

  ElementType element[] = {1};
  EXPECT_OK(fifo_a.write(kElementSize, &element, std::size(element), &actual_count));

  EXPECT_STATUS(fifo_b.read(kElementSize, buffer, std::size(element), &actual_count),
                ZX_ERR_INVALID_ARGS);
}

TEST(FifoTest, WriteBadBuffer) {
  const size_t kVmoSize = zx_system_get_page_size();
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kVmoSize, 0, &vmo));

  zx_vaddr_t addr;

  // Note, no options means the buffer is not readable.
  ASSERT_OK(zx::vmar::root_self()->map(0, 0, vmo, 0, kVmoSize, &addr));
  auto unmap = fit::defer([&]() { zx::vmar::root_self()->unmap(addr, kVmoSize); });

  void* buffer = reinterpret_cast<void*>(addr);

  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(4, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;

  ElementType element[] = {1};
  EXPECT_STATUS(fifo_a.write(kElementSize, buffer, std::size(element), &actual_count),
                ZX_ERR_INVALID_ARGS);
}

TEST(FifoTest, ReadPartialBadBuffer) {
  const size_t kVmoSize = zx_system_get_page_size();
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kVmoSize, 0, &vmo));

  VmoMapWithPadding map;
  zx_vaddr_t addr;
  ASSERT_OK(map.Map(vmo, kVmoSize, &addr));

  // Calculate buffer such that 1 element will fit, and the next will be out of bounds.
  void* buffer = reinterpret_cast<void*>(addr + kVmoSize - sizeof(ElementType));

  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(4, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;

  ElementType element[] = {1, 2};
  EXPECT_OK(fifo_a.write(kElementSize, &element, std::size(element), &actual_count));

  EXPECT_STATUS(fifo_b.read(kElementSize, buffer, std::size(element), &actual_count),
                ZX_ERR_INVALID_ARGS);
}

TEST(FifoTest, WritePartialBadBuffer) {
  const size_t kVmoSize = zx_system_get_page_size();
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kVmoSize, 0, &vmo));

  VmoMapWithPadding map;
  zx_vaddr_t addr;
  ASSERT_OK(map.Map(vmo, kVmoSize, &addr));

  // Calculate buffer such that 1 element will fit, and the next will be out of bounds.
  void* buffer = reinterpret_cast<void*>(addr + kVmoSize - sizeof(ElementType));

  zx::fifo fifo_a, fifo_b;
  ASSERT_OK(zx::fifo::create(4, kElementSize, 0, &fifo_a, &fifo_b));

  size_t actual_count;

  ElementType element[] = {1, 2};
  EXPECT_STATUS(fifo_a.write(kElementSize, buffer, std::size(element), &actual_count),
                ZX_ERR_INVALID_ARGS);
}

}  // namespace
