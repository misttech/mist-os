// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_MOCK_MMIO_CPP_MOCK_MMIO_REG_H_
#define LIB_DRIVER_MOCK_MMIO_CPP_MOCK_MMIO_REG_H_

#include <lib/mmio/mmio.h>

#include <memory>

namespace mock_mmio {

// Mocks a single MMIO register. This class is intended to be used with a fdf::MmioBuffer;
// operations on an instance of that class will be directed to the mock if the mock-mmio-reg library
// is a dependency of the test.
class MockMmioReg {
 public:
  // Reads from the mocked register. Returns the value set by the next expectation, or the default
  // value. The default is initially zero and can be set by calling ReadReturns() or Write(). This
  // method is expected to be called (indirectly) by the code under test.
  uint64_t Read();

  // Writes to the mocked register. This method is expected to be called (indirectly) by the code
  // under test.
  void Write(uint64_t value);

  // Matches a register read and returns the specified value.
  MockMmioReg& ExpectRead(uint64_t value);

  // Matches a register read and returns the default value.
  MockMmioReg& ExpectRead();

  // Sets the default register read value.
  MockMmioReg& ReadReturns(uint64_t value);

  // Matches a register write with the specified value.
  MockMmioReg& ExpectWrite(uint64_t value);

  // Matches any register write.
  MockMmioReg& ExpectWrite();

  // Removes and ignores all expectations and resets the default read value.
  void Clear();

  // Removes all expectations and resets the default value. The presence of any outstanding
  // expectations causes a test failure.
  void VerifyAndClear();

 private:
  struct MmioExpectation {
    enum class Match { kEqual, kAny } match;
    uint64_t value;
  };

  uint64_t last_value_ = 0;

  size_t read_expectations_index_ = 0;
  std::vector<MmioExpectation> read_expectations_;

  size_t write_expectations_index_ = 0;
  std::vector<MmioExpectation> write_expectations_;
};

// Mocks a region of MMIO registers. Each register is backed by a MockMmioReg instance.
//
// Example:
// mock_mmio::MockMmioRegRegion mock_registers(register_size, number_of_registers);
// fdf::MmioBuffer mmio_buffer(mock_registers.GetMmioBuffer());
//
// SomeDriver dut(mmio_buffer);
// mock_registers[0]
//     .ExpectRead()
//     .ExpectWrite(0xdeadbeef)
//     .ExpectRead(0xcafecafe)
//     .ExpectWrite()
//     .ExpectRead();
// mock_registers[5]
//     .ExpectWrite(0)
//     .ExpectWrite(1024)
//     .ReadReturns(0);
//
// EXPECT_OK(dut.SomeMethod());
// mock_registers.VerifyAll();
//
class MockMmioRegRegion {
 public:
  // Constructs a MockMmioRegRegion. reg_size is the size of each register in bytes, and reg_count
  // is the total number of registers. If all accesses will be to registers past a certain address,
  // reg_offset can be set to this value (in number of registers) to reduce the required reg_count.
  // Accesses to registers lower than this offset are not permitted.
  MockMmioRegRegion(size_t reg_size, size_t reg_count, size_t reg_offset = 0)
      : reg_size_(reg_size), reg_count_(reg_count), reg_offset_(reg_offset) {
    regs_.resize(reg_count_);
  }

  // Accesses the MockMmioReg at the given offset. Note that this is the _offset_, not the
  // _index_.
  const MockMmioReg& operator[](size_t offset) const {
    CheckOffset(offset);
    return regs_[(offset / reg_size_) - reg_offset_];
  }

  // Accesses the MockMmioReg at the given offset. Note that this is the offset, not the
  // index.
  MockMmioReg& operator[](size_t offset) {
    CheckOffset(offset);
    return regs_[(offset / reg_size_) - reg_offset_];
  }

  // Calls VerifyAndClear() on all MockMmioReg objects.
  void VerifyAll();

  fdf::MmioBuffer GetMmioBuffer();

 private:
  static uint8_t Read8(const void* ctx, const mmio_buffer_t& mmio, zx_off_t offs) {
    auto& reg_region = *reinterpret_cast<MockMmioRegRegion*>(const_cast<void*>(ctx));
    return static_cast<uint8_t>(reg_region[offs + mmio.offset].Read());
  }

  static uint16_t Read16(const void* ctx, const mmio_buffer_t& mmio, zx_off_t offs) {
    auto& reg_region = *reinterpret_cast<MockMmioRegRegion*>(const_cast<void*>(ctx));
    return static_cast<uint16_t>(reg_region[offs + mmio.offset].Read());
  }

  static uint32_t Read32(const void* ctx, const mmio_buffer_t& mmio, zx_off_t offs) {
    auto& reg_region = *reinterpret_cast<MockMmioRegRegion*>(const_cast<void*>(ctx));
    return static_cast<uint32_t>(reg_region[offs + mmio.offset].Read());
  }

  static uint64_t Read64(const void* ctx, const mmio_buffer_t& mmio, zx_off_t offs) {
    auto& reg_region = *reinterpret_cast<MockMmioRegRegion*>(const_cast<void*>(ctx));
    return reg_region[offs + mmio.offset].Read();
  }

  static void Write8(const void* ctx, const mmio_buffer_t& mmio, uint8_t val, zx_off_t offs) {
    Write64(ctx, mmio, val, offs);
  }

  static void Write16(const void* ctx, const mmio_buffer_t& mmio, uint16_t val, zx_off_t offs) {
    Write64(ctx, mmio, val, offs);
  }

  static void Write32(const void* ctx, const mmio_buffer_t& mmio, uint32_t val, zx_off_t offs) {
    Write64(ctx, mmio, val, offs);
  }

  static void Write64(const void* ctx, const mmio_buffer_t& mmio, uint64_t val, zx_off_t offs) {
    auto& reg_region = *reinterpret_cast<MockMmioRegRegion*>(const_cast<void*>(ctx));
    reg_region[offs + mmio.offset].Write(val);
  }

  static constexpr fdf::MmioBufferOps kMockMmioOps = {
      .Read8 = Read8,
      .Read16 = Read16,
      .Read32 = Read32,
      .Read64 = Read64,
      .Write8 = Write8,
      .Write16 = Write16,
      .Write32 = Write32,
      .Write64 = Write64,
  };

  void CheckOffset(zx_off_t offsets) const;

  std::vector<MockMmioReg> regs_;
  const size_t reg_size_;
  const size_t reg_count_;
  const size_t reg_offset_;
};

}  // namespace mock_mmio

#endif  // LIB_DRIVER_MOCK_MMIO_CPP_MOCK_MMIO_REG_H_
