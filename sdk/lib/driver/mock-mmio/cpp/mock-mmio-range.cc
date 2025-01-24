// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/mock-mmio/cpp/mock-mmio-range.h>

namespace mock_mmio {

namespace {
fdf::MmioBuffer CreateMmioBuffer(size_t size, uint32_t cache_policy, const fdf::MmioBufferOps* ops,
                                 void* ctx) {
  zx::vmo vmo;
  ZX_ASSERT(zx::vmo::create(/*size=*/size, 0, &vmo) == ZX_OK);
  mmio_buffer_t mmio{};
  ZX_ASSERT(mmio_buffer_init(&mmio, 0, size, vmo.release(), cache_policy) == ZX_OK);
  return fdf::MmioBuffer(mmio, ops, ctx);
}
}  // namespace

void MockMmioRange::Expect(cpp20::span<const MockMmioRange::Access> accesses) {
  const std::lock_guard<std::mutex> lock(mutex_);
  for (const auto& access : accesses) {
    access_list_.push_back({
        .address = access.address,
        .value = access.value,
        .write = access.write,
        .size =
            access.size == MockMmioRange::Size::kUseDefault ? default_access_size_ : access.size,
    });
  }
}

void MockMmioRange::CheckAllAccessesReplayed() {
  const std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT(access_list_.size() == access_index_);
}

fdf::MmioBuffer MockMmioRange::GetMmioBuffer() {
  static constexpr fdf::MmioBufferOps kMockMmioOps = {
      .Read8 = MockMmioRange::Read8,
      .Read16 = MockMmioRange::Read16,
      .Read32 = MockMmioRange::Read32,
      .Read64 = MockMmioRange::Read64,
      .Write8 = MockMmioRange::Write8,
      .Write16 = MockMmioRange::Write16,
      .Write32 = MockMmioRange::Write32,
      .Write64 = MockMmioRange::Write64,
  };

  return CreateMmioBuffer(range_size_, ZX_CACHE_POLICY_CACHED, &kMockMmioOps, this);
}

uint64_t MockMmioRange::Read(zx_off_t address, MockMmioRange::Size size) const {
  const std::lock_guard<std::mutex> lock(mutex_);
  if (access_index_ >= access_list_.size()) {
    return 0;
  }

  const MockMmioRange::Access& expected_access = access_list_[access_index_];
  ++access_index_;

  ZX_ASSERT(expected_access.address == address);
  ZX_ASSERT(expected_access.write == false);
  return expected_access.value;
}

void MockMmioRange::Write(zx_off_t address, uint64_t value, MockMmioRange::Size size) const {
  const std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT(access_index_ < access_list_.size());

  const MockMmioRange::Access& expected_access = access_list_[access_index_];
  ++access_index_;

  ZX_ASSERT(expected_access.address == address);
  ZX_ASSERT(expected_access.value == value);
  ZX_ASSERT(expected_access.size == size);
}

}  // namespace mock_mmio
