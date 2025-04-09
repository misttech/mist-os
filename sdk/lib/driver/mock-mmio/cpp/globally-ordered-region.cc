// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/mock-mmio/cpp/globally-ordered-region.h>

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

void GloballyOrderedRegion::Expect(cpp20::span<const GloballyOrderedRegion::Access> accesses) {
  const std::lock_guard<std::mutex> lock(mutex_);
  for (const auto& access : accesses) {
    access_list_.push_back({
        .address = access.address,
        .value = access.value,
        .write = access.write,
        .size = access.size == GloballyOrderedRegion::Size::kUseDefault ? default_access_size_
                                                                        : access.size,
    });
  }
}

void GloballyOrderedRegion::CheckAllAccessesReplayed() {
  const std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT(access_list_.size() == access_index_);
}

fdf::MmioBuffer GloballyOrderedRegion::GetMmioBuffer() {
  static constexpr fdf::MmioBufferOps kMockMmioOps = {
      .Read8 = GloballyOrderedRegion::Read8,
      .Read16 = GloballyOrderedRegion::Read16,
      .Read32 = GloballyOrderedRegion::Read32,
      .Read64 = GloballyOrderedRegion::Read64,
      .Write8 = GloballyOrderedRegion::Write8,
      .Write16 = GloballyOrderedRegion::Write16,
      .Write32 = GloballyOrderedRegion::Write32,
      .Write64 = GloballyOrderedRegion::Write64,
  };

  return CreateMmioBuffer(range_size_, ZX_CACHE_POLICY_CACHED, &kMockMmioOps, this);
}

uint64_t GloballyOrderedRegion::Read(zx_off_t address, GloballyOrderedRegion::Size size) const {
  const std::lock_guard<std::mutex> lock(mutex_);
  if (access_index_ >= access_list_.size()) {
    return 0;
  }

  const GloballyOrderedRegion::Access& expected_access = access_list_[access_index_];
  ++access_index_;

  ZX_ASSERT(expected_access.address == address);
  ZX_ASSERT(expected_access.write == false);
  return expected_access.value;
}

void GloballyOrderedRegion::Write(zx_off_t address, uint64_t value,
                                  GloballyOrderedRegion::Size size) const {
  const std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT(access_index_ < access_list_.size());

  const GloballyOrderedRegion::Access& expected_access = access_list_[access_index_];
  ++access_index_;

  ZX_ASSERT(expected_access.address == address);
  ZX_ASSERT(expected_access.value == value);
  ZX_ASSERT(expected_access.size == size);
}

}  // namespace mock_mmio
