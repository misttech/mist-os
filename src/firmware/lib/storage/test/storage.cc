// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "storage.h"

#include <gtest/gtest.h>

namespace {

class AlignedBuffer {
 public:
  AlignedBuffer(size_t size) : buffer_(size + FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT) {}

  uint8_t* get() {
    uintptr_t addr = reinterpret_cast<uintptr_t>(buffer_.data());
    addr = (addr + FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT - 1) /
           FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT * FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT;
    return reinterpret_cast<uint8_t*>(addr);
  }

 private:
  std::vector<uint8_t> buffer_;
};

class TestFuchsiaFirmwareStorage {
 public:
  TestFuchsiaFirmwareStorage(size_t size, size_t block_size)
      : scratch_buffer_(block_size), block_size_(block_size) {
    // Initialize buffer data;
    for (size_t i = 0; i < size; i++) {
      buffer_.push_back(static_cast<uint8_t>(i));
    }
  }

  FuchsiaFirmwareStorage GetFuchsiaFirmwareStorage() {
    return {
        block_size_, buffer_.size() / block_size_, scratch_buffer_.get(), this, Read, Write,
    };
  }

  const std::vector<uint8_t>& buffer() { return buffer_; }

 private:
  static bool Read(void* ctx, size_t block_offset, size_t blocks_count, void* dst) {
    TestFuchsiaFirmwareStorage* ptr = static_cast<TestFuchsiaFirmwareStorage*>(ctx);
    size_t offset = block_offset * ptr->block_size_, size = blocks_count * ptr->block_size_;
    if ((reinterpret_cast<uintptr_t>(dst) % FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT) ||
        offset + size > ptr->buffer_.size()) {
      return false;
    }
    memcpy(dst, ptr->buffer_.data() + offset, size);
    return true;
  }

  static bool Write(void* ctx, size_t block_offset, size_t blocks_count, const void* src) {
    TestFuchsiaFirmwareStorage* ptr = static_cast<TestFuchsiaFirmwareStorage*>(ctx);
    size_t offset = block_offset * ptr->block_size_, size = blocks_count * ptr->block_size_;
    if ((reinterpret_cast<uintptr_t>(src) % FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT) ||
        offset + size > ptr->buffer_.size()) {
      return false;
    }
    memcpy(ptr->buffer_.data() + offset, src, size);
    return true;
  }

  AlignedBuffer scratch_buffer_;
  std::vector<uint8_t> buffer_;
  size_t block_size_;
};

struct FuchsiaFirmwareStorageTestCase {
  size_t offset;
  size_t size;
  size_t buffer_offset;
  size_t block_size;
  size_t storage_size;

  std::string Name() const {
    static char buffer[512];
    snprintf(buffer, sizeof(buffer),
             "offset%zu_size%zu_buffer_offset%zu_block_size%zu_storage_size%zu", offset, size,
             buffer_offset, block_size, storage_size);
    return std::string(buffer);
  }
};

using FuchsiaFirmwareStorageTest = ::testing::TestWithParam<FuchsiaFirmwareStorageTestCase>;

template <size_t block_size>
struct FuchsiaFirmwareStorageTestCases {
  static constexpr size_t kStorageSize = 3 * block_size;
  static constexpr FuchsiaFirmwareStorageTestCase kTestCases[] = {
      // offset
      //   |~~~~~~~~~~~~~size~~~~~~~~~~~~|
      //   |---------|---------|---------|
      {0, kStorageSize, 0, block_size, kStorageSize},
      // offset
      //   |~~~~~~~~~size~~~~~~~~~|
      //   |---------|---------|---------|
      {0, kStorageSize - 1, 0, block_size, kStorageSize},
      // offset
      //   |~~size~~|
      //   |---------|---------|---------|
      {0, block_size - 1, 0, block_size, kStorageSize},
      //     offset
      //       |~~~~~~~~~~~size~~~~~~~~~~|
      //   |---------|---------|---------|
      {1, kStorageSize - 1, 0, block_size, kStorageSize},
      //     offset
      //       |~~~~~~~~~size~~~~~~~~|
      //   |---------|---------|---------|
      {1, kStorageSize - 2, 0, block_size, kStorageSize},
      //     offset
      //       |~~~size~~~|
      //   |---------|---------|---------|
      {1, block_size, 0, block_size, kStorageSize},
      //   offset
      //     |~size~|
      //   |---------|---------|---------|
      {1, block_size - 2, 0, block_size, kStorageSize},

      // Run the same with an additional offset of one block
      {block_size, kStorageSize, 0, block_size, kStorageSize + block_size},
      {block_size, kStorageSize - 1, 0, block_size, kStorageSize + block_size},
      {block_size, block_size - 1, 0, block_size, kStorageSize + block_size},
      {block_size + 1, kStorageSize - 1, 0, block_size, kStorageSize + block_size},
      {block_size + 1, kStorageSize - 2, 0, block_size, kStorageSize + block_size},
      {block_size + 1, block_size, 0, block_size, kStorageSize + block_size},
      {block_size + 1, block_size - 2, 0, block_size, kStorageSize + block_size},

      // Run the same with unaligned output address
      {0, kStorageSize, 1, block_size, kStorageSize},
      {0, kStorageSize - 1, 1, block_size, kStorageSize},
      {0, block_size - 1, 1, block_size, kStorageSize},
      {1, kStorageSize - 1, 1, block_size, kStorageSize},
      {1, kStorageSize - 2, 1, block_size, kStorageSize},
      {1, block_size, 1, block_size, kStorageSize},
      {1, block_size - 2, 1, block_size, kStorageSize},

      // A special case where the value of `offset` is not block aligned but DMA aligned. This can
      // trigger some internal optimization code path.
      {FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT,
       kStorageSize - FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT, 0, block_size, kStorageSize},
      {FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT,
       kStorageSize - FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT - 1, 0, block_size, kStorageSize},
      {FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT, block_size, 0, block_size, kStorageSize},
      {FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT,
       block_size - FUCHSIA_FIRMWARE_STORAGE_BUFFER_ALIGNMENT - 1, 0, block_size, kStorageSize},
  };
};

INSTANTIATE_TEST_SUITE_P(
    FuchsiaFirmwareStorageTestsBlockSize512, FuchsiaFirmwareStorageTest,
    testing::ValuesIn<FuchsiaFirmwareStorageTestCase>(
        FuchsiaFirmwareStorageTestCases<512>::kTestCases),
    [](const testing::TestParamInfo<FuchsiaFirmwareStorageTest::ParamType>& info) {
      return info.param.Name();
    });

INSTANTIATE_TEST_SUITE_P(
    FuchsiaFirmwareStorageTestsBlockSize4096, FuchsiaFirmwareStorageTest,
    testing::ValuesIn<FuchsiaFirmwareStorageTestCase>(
        FuchsiaFirmwareStorageTestCases<4096>::kTestCases),
    [](const testing::TestParamInfo<FuchsiaFirmwareStorageTest::ParamType>& info) {
      return info.param.Name();
    });

TEST_P(FuchsiaFirmwareStorageTest, FuchsiaFirmwareStorageReadTestParameterized) {
  const FuchsiaFirmwareStorageTestCase& test_case = GetParam();
  TestFuchsiaFirmwareStorage storage(test_case.storage_size, test_case.block_size);
  FuchsiaFirmwareStorage ops = storage.GetFuchsiaFirmwareStorage();
  AlignedBuffer read_buffer(test_case.size + test_case.buffer_offset);
  ASSERT_TRUE(FuchsiaFirmwareStorageRead(&ops, test_case.offset, test_case.size,
                                         read_buffer.get() + test_case.buffer_offset));
  ASSERT_EQ(memcmp(storage.buffer().data() + test_case.offset,
                   read_buffer.get() + test_case.buffer_offset, test_case.size),
            0);
}

}  // namespace
