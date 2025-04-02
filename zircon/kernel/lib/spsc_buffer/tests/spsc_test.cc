// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/spsc_buffer/spsc_buffer.h>
#include <lib/unittest/unittest.h>

#include <fbl/alloc_checker.h>
#include <ktl/array.h>
#include <ktl/byte.h>
#include <ktl/span.h>

// The SpscBufferTests class is a friend of the SpscBuffer class, thus allowing tests to access
// private members of that class.
class SpscBufferTests {
 public:
  // Test that splitting combined pointers into the constituent read and write pointers works.
  static bool TestSplitCombinedPointers() {
    BEGIN_TEST;

    struct TestCase {
      uint64_t combined_pointers;
      SpscBuffer::RingPointers expected;
    };
    constexpr ktl::array kTestCases = ktl::to_array<TestCase>({
        // Basic case.
        {
            .combined_pointers = 0x1320087'01005382,
            .expected = {.read = 0x1320087, .write = 0x1005382},
        },
        // Edge case where the pointers are 0.
        {
            .combined_pointers = 0,
            .expected = {.read = 0, .write = 0},
        },
        // Edge case where the highest bit of the read pointer is set.
        {
            .combined_pointers = 0xFFFFFFFF'00004587,
            .expected = {.read = 0xFFFFFFFF, .write = 0x4587},
        },
        // Edge case where the highest bit of the write pointer is set.
        {
            .combined_pointers = 0x4587'FFFFFFFF,
            .expected = {.read = 0x4587, .write = 0xFFFFFFFF},
        },
    });

    for (const TestCase& tc : kTestCases) {
      const SpscBuffer::RingPointers actual =
          SpscBuffer::SplitCombinedPointers(tc.combined_pointers);
      EXPECT_EQ(tc.expected.read, actual.read);
      EXPECT_EQ(tc.expected.write, actual.write);
    }

    END_TEST;
  }

  // Test that combining read and write pointers into a single combined pointer works.
  static bool TestCombinePointers() {
    BEGIN_TEST;

    struct TestCase {
      SpscBuffer::RingPointers pointers;
      uint64_t combined;
    };
    constexpr ktl::array kTestCases = ktl::to_array<TestCase>({
        // Basic case.
        {
            .pointers = {.read = 0x1320087, .write = 0x1005382},
            .combined = 0x1320087'01005382,
        },
        // Edge case where the read and write pointers are 0.
        {
            .pointers = {.read = 0, .write = 0},
            .combined = 0,
        },
        // Edge case where the highest bit of the read pointer is set.
        {
            .pointers = {.read = 0xFFFFFFFF, .write = 0x123},
            .combined = 0xFFFFFFFF'00000123,
        },
        // Edge case where the highest bit of the write pointer is set.
        {
            .pointers = {.read = 0x123, .write = 0xFFFFFFFF},
            .combined = 0x123'FFFFFFFF,
        },
    });

    for (const TestCase& tc : kTestCases) {
      const uint64_t actual = SpscBuffer::CombinePointers(tc.pointers);
      EXPECT_EQ(tc.combined, actual);
    }

    END_TEST;
  }

  // Test that the amount of available space in the ring buffer is correctly calculated.
  static bool TestAvailableSpace() {
    BEGIN_TEST;

    struct TestCase {
      SpscBuffer::RingPointers pointers;
      size_t buffer_size;
      uint32_t expected;
    };
    constexpr ktl::array kTestCases = ktl::to_array<TestCase>({
        // A completely uninitialized buffer should return an available space of 0.
        {
            .pointers = {.read = 0, .write = 0},
            .buffer_size = 0,
            .expected = 0,
        },
        // The next two cases verify that the available space is the size of the storage buffer when
        // the read and write pointers are equal.
        {
            .pointers = {.read = 0, .write = 0},
            .buffer_size = 16,
            .expected = 16,
        },
        {
            .pointers = {.read = 3, .write = 3},
            .buffer_size = 16,
            .expected = 16,
        },
        // Test that the available space is computed correctly when the write pointer is in front of
        // the read pointer.
        {
            .pointers = {.read = 3, .write = 7},
            .buffer_size = 16,
            .expected = 12,
        },
        // Test that the available space is computed correctly when the write pointer overflows and
        // wraps around.
        {
            .pointers = {.read = 0xFFFFFFFC, .write = 3},
            .buffer_size = 16,
            .expected = 9,
        },
        // Test that the available space is computed correctly when the buffer is full.
        {
            .pointers = {.read = 0, .write = 16},
            .buffer_size = 16,
            .expected = 0,
        },
    });

    for (const TestCase& tc : kTestCases) {
      // Create a buffer to test with. The buffer's backing storage is initialized using a null span
      // in all of these tests. This would be invalid if we were reading from or writing to the
      // buffer, but all we're doing is using the provided size to compute the amount of available
      // space.
      SpscBuffer buffer;
      ktl::span<ktl::byte> null_span(static_cast<ktl::byte*>(nullptr), tc.buffer_size);
      buffer.Init(null_span);

      const uint32_t actual = buffer.AvailableSpace(tc.pointers);
      EXPECT_EQ(tc.expected, actual);
    }

    END_TEST;
  }

  // Test that the amount of available data in the ring buffer is correctly calculated.
  static bool TestAvailableData() {
    BEGIN_TEST;

    struct TestCase {
      SpscBuffer::RingPointers pointers;
      size_t buffer_size;
      uint32_t expected;
    };
    constexpr ktl::array kTestCases = ktl::to_array<TestCase>({
        // A completely uninitialized buffer should return an available data of 0.
        {
            .pointers = {.read = 0, .write = 0},
            .buffer_size = 0,
            .expected = 0,
        },
        // The next two cases verify that the available data is 0 when the read and write pointers
        // are equal.
        {
            .pointers = {.read = 0, .write = 0},
            .buffer_size = 16,
            .expected = 0,
        },
        {
            .pointers = {.read = 3, .write = 3},
            .buffer_size = 16,
            .expected = 0,
        },
        // Test that the available data is computed correctly when the write pointer is in front of
        // the read pointer.
        {
            .pointers = {.read = 3, .write = 7},
            .buffer_size = 16,
            .expected = 4,
        },
        // Test that the available data is computed correctly when the buffer is full.
        {
            .pointers = {.read = 0, .write = 16},
            .buffer_size = 16,
            .expected = 16,
        },
        // Test that the available data is computed correctly when the write pointer overflows and
        // wraps around.
        {
            .pointers = {.read = 0xFFFFFFFC, .write = 0x3},
            .buffer_size = 16,
            .expected = 7,
        },
    });

    for (const TestCase& tc : kTestCases) {
      const uint32_t actual = SpscBuffer::AvailableData(tc.pointers);
      EXPECT_EQ(tc.expected, actual);
    }

    END_TEST;
  }

  // Test that advancing the read pointer works as expected.
  static bool TestAdvanceReadPointer() {
    BEGIN_TEST;

    struct TestCase {
      SpscBuffer::RingPointers initial_pointers;
      uint64_t buffer_size;
      uint32_t delta;
      uint64_t expected;
    };

    constexpr ktl::array kTestCases = ktl::to_array<TestCase>({
        // Basic case.
        {
            .initial_pointers = {.read = 1, .write = 6},
            .buffer_size = 16,
            .delta = 4,
            .expected = 0x5'00000006,
        },
        // Test that the read pointer can be equal to the size of the buffer.
        {
            .initial_pointers = {.read = 0, .write = 16},
            .buffer_size = 16,
            .delta = 16,
            .expected = 0x10'00000010,
        },
        // Test that the read pointer wrapping works correctly.
        {
            .initial_pointers = {.read = 0xFFFFFFFC, .write = 12},
            .buffer_size = 16,
            .delta = 5,
            .expected = 0x1'0000000C,
        },
    });

    for (const TestCase& tc : kTestCases) {
      // Create a buffer to test with. The buffer's backing storage is initialized using a null span
      // in all of these tests. This would be invalid if we were reading from or writing to the
      // buffer, but all we're doing is advancing the read pointer and making sure it's valid.
      SpscBuffer buffer;
      ktl::span<ktl::byte> null_span(static_cast<ktl::byte*>(nullptr), tc.buffer_size);
      buffer.Init(null_span);

      // Set the initial value of the combined pointers.
      const uint64_t initial = SpscBuffer::CombinePointers(tc.initial_pointers);
      buffer.combined_pointers_.store(initial, ktl::memory_order_release);

      // Advance the read pointer and ensure that combined_pointers_ is correct.
      buffer.AdvanceReadPointer(tc.initial_pointers, tc.delta);
      const uint64_t actual = buffer.combined_pointers_.load(ktl::memory_order_acquire);
      EXPECT_EQ(tc.expected, actual);
    }

    END_TEST;
  }

  // Test that advancing the write pointer works as expected.
  static bool TestAdvanceWritePointer() {
    BEGIN_TEST;

    struct TestCase {
      SpscBuffer::RingPointers initial_pointers;
      uint64_t buffer_size;
      uint32_t delta;
      uint64_t expected;
    };

    constexpr ktl::array kTestCases = ktl::to_array<TestCase>({
        // Test that the read pointer is preserved.
        {
            .initial_pointers = {.read = 7, .write = 9},
            .buffer_size = 16,
            .delta = 4,
            .expected = 0x7'0000000D,
        },
        // Test that the write pointer can be equal to the size of the buffer.
        {
            .initial_pointers = {.read = 0, .write = 0},
            .buffer_size = 16,
            .delta = 16,
            .expected = 0x10,
        },
        // Test that the write pointer wrapping works correctly.
        {
            .initial_pointers = {.read = 12, .write = 0xFFFFFFFC},
            .buffer_size = 16,
            .delta = 5,
            .expected = 0xC'00000001,
        },
    });

    for (const TestCase& tc : kTestCases) {
      // Create a buffer to test with. The buffer's backing storage is initialized using a null span
      // in all of these tests. This would be invalid if we were reading from or writing to the
      // buffer, but all we're doing is advancing the write pointer and making sure it's valid.
      SpscBuffer buffer;
      ktl::span<ktl::byte> null_span(static_cast<ktl::byte*>(nullptr), tc.buffer_size);
      buffer.Init(null_span);

      // Set the initial value of the combined pointers.
      const uint64_t initial = SpscBuffer::CombinePointers(tc.initial_pointers);
      buffer.combined_pointers_.store(initial, ktl::memory_order_release);

      // Advance the write pointer and ensure that combined_pointers_ is correct.
      buffer.AdvanceWritePointer(tc.initial_pointers, tc.delta);
      const uint64_t actual = buffer.combined_pointers_.load(ktl::memory_order_acquire);
      EXPECT_EQ(tc.expected, actual);
    }

    END_TEST;
  }
};

UNITTEST_START_TESTCASE(spsc_buffer_tests)
UNITTEST("split_pointers", SpscBufferTests::TestSplitCombinedPointers)
UNITTEST("combine_pointers", SpscBufferTests::TestCombinePointers)
UNITTEST("available_space", SpscBufferTests::TestAvailableSpace)
UNITTEST("available_data", SpscBufferTests::TestAvailableData)
UNITTEST("advance_read_pointer", SpscBufferTests::TestAdvanceReadPointer)
UNITTEST("advance_write_pointer", SpscBufferTests::TestAdvanceWritePointer)
UNITTEST_END_TESTCASE(spsc_buffer_tests, "spsc_buffer",
                      "Test the single-producer, single-consumer ring buffer implementation.")
