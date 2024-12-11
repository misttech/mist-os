// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/mock-mmio-reg/cpp/mock-mmio-reg.h>

namespace mock_mmio {

// Reads from the mocked register. Returns the value set by the next expectation, or the default
// value. The default is initially zero and can be set by calling ReadReturns() or Write(). This
// method is expected to be called (indirectly) by the code under test.
uint64_t MockMmioReg::Read() {
  if (read_expectations_index_ >= read_expectations_.size()) {
    return last_value_;
  }

  MmioExpectation& exp = read_expectations_[read_expectations_index_++];
  if (exp.match == MmioExpectation::Match::kAny) {
    return last_value_;
  }

  return last_value_ = exp.value;
}

// Writes to the mocked register. This method is expected to be called (indirectly) by the code
// under test.
void MockMmioReg::Write(uint64_t value) {
  last_value_ = value;

  if (write_expectations_index_ >= write_expectations_.size()) {
    return;
  }

  MockMmioReg::MmioExpectation& exp = write_expectations_[write_expectations_index_++];
  if (exp.match != MmioExpectation::Match::kAny) {
    ZX_ASSERT(exp.value == value);
  }
}

// Matches a register read and returns the specified value.
MockMmioReg& MockMmioReg::ExpectRead(uint64_t value) {
  read_expectations_.push_back(
      MmioExpectation{.match = MmioExpectation::Match::kEqual, .value = value});

  return *this;
}

// Matches a register read and returns the default value.
MockMmioReg& MockMmioReg::ExpectRead() {
  read_expectations_.push_back(MmioExpectation{.match = MmioExpectation::Match::kAny, .value = 0});

  return *this;
}

// Sets the default register read value.
MockMmioReg& MockMmioReg::ReadReturns(uint64_t value) {
  last_value_ = value;
  return *this;
}

// Matches a register write with the specified value.
MockMmioReg& MockMmioReg::ExpectWrite(uint64_t value) {
  write_expectations_.push_back(
      MmioExpectation{.match = MmioExpectation::Match::kEqual, .value = value});

  return *this;
}

// Matches any register write.
MockMmioReg& MockMmioReg::ExpectWrite() {
  write_expectations_.push_back(MmioExpectation{.match = MmioExpectation::Match::kAny, .value = 0});

  return *this;
}

void MockMmioReg::Clear() {
  last_value_ = 0;

  read_expectations_index_ = 0;
  while (read_expectations_.size() > 0) {
    read_expectations_.pop_back();
  }

  write_expectations_index_ = 0;
  while (write_expectations_.size() > 0) {
    write_expectations_.pop_back();
  }
}

void MockMmioReg::VerifyAndClear() {
  ZX_ASSERT(read_expectations_index_ >= read_expectations_.size());
  ZX_ASSERT(write_expectations_index_ >= write_expectations_.size());
  Clear();
}

void MockMmioRegRegion::VerifyAll() {
  for (auto& reg : regs_) {
    reg.VerifyAndClear();
  }
}

fdf::MmioBuffer MockMmioRegRegion::GetMmioBuffer() {
  size_t size = (reg_offset_ + reg_count_) * reg_size_;

  // TODO(https://fxbug.dev/335906001): This creates an unneeded VMO for now.
  zx::vmo vmo;
  ZX_ASSERT(zx::vmo::create(/*size=*/size, 0, &vmo) == ZX_OK);
  mmio_buffer_t mmio{};
  ZX_ASSERT(mmio_buffer_init(&mmio, 0, size, vmo.release(), ZX_CACHE_POLICY_CACHED) == ZX_OK);
  return fdf::MmioBuffer(mmio, &MockMmioRegRegion::kMockMmioOps, this);
}

void MockMmioRegRegion::CheckOffset(zx_off_t offsets) const {
  ZX_ASSERT(offsets / reg_size_ >= reg_offset_);
  ZX_ASSERT((offsets / reg_size_) - reg_offset_ < reg_count_);
}

}  // namespace mock_mmio
