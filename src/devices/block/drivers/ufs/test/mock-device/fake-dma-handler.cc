// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-dma-handler.h"

#include <lib/fit/defer.h>
#include <lib/zx/vmar.h>

namespace ufs {
namespace ufs_mock_device {

FakeDmaHandler::FakeDmaHandler() {
  for (size_t i = 0; i < std::size(fake_bti_paddrs_); i++) {
    fake_bti_paddrs_[i] = FAKE_BTI_PHYS_ADDR * (i + 1);
  }

  fake_bti_create_with_paddrs(fake_bti_paddrs_, kFakeBtiAddrsCount,
                              fake_bti_.reset_and_get_address());
}

FakeDmaHandler::~FakeDmaHandler() {
  for (const auto& mapped_addr : mapped_addrs_) {
    zx::vmar::root_self()->unmap(mapped_addr.first, mapped_addr.second);
  }
}

zx::result<zx_vaddr_t> FakeDmaHandler::PhysToVirt(zx_paddr_t paddr) {
  if (paddr % FAKE_BTI_PHYS_ADDR != 0 || paddr == 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (paddr > FAKE_BTI_PHYS_ADDR * kFakeBtiAddrsCount) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  size_t vmo_info_num;
  std::vector<fake_bti_pinned_vmo_info_t> vmo_infos(kFakeBtiAddrsCount);
  if (auto status = fake_bti_get_pinned_vmos(fake_bti_.get(), vmo_infos.data(), kFakeBtiAddrsCount,
                                             &vmo_info_num);
      status != ZX_OK) {
    return zx::error(status);
  }

  uint32_t vmo_info_index;
  auto defer = fit::defer([&]() {
    // Close all remaining handles
    for (++vmo_info_index; vmo_info_index < vmo_info_num; ++vmo_info_index) {
      zx_handle_close(vmo_infos[vmo_info_index].vmo);
    }
  });

  for (vmo_info_index = 0; vmo_info_index < vmo_info_num; ++vmo_info_index) {
    auto vmo = zx::vmo(vmo_infos[vmo_info_index].vmo);
    size_t num_paddrs;
    std::vector<zx_paddr_t> paddrs(kFakeBtiAddrsCount);
    if (auto status =
            fake_bti_get_phys_from_pinned_vmo(fake_bti_.get(), vmo_infos[vmo_info_index],
                                              paddrs.data(), kFakeBtiAddrsCount, &num_paddrs);
        status != ZX_OK) {
      return zx::error(status);
    }

    for (uint32_t paddr_index = 0; paddr_index < num_paddrs; ++paddr_index) {
      if (paddr == paddrs[paddr_index]) {
        zx_vaddr_t vaddr;
        if (auto status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo,
                                                     vmo_infos[vmo_info_index].offset,
                                                     vmo_infos[vmo_info_index].size, &vaddr);
            status != ZX_OK) {
          return zx::error(status);
        }
        mapped_addrs_.push_back(std::make_pair(vaddr, vmo_infos[vmo_info_index].size));
        return zx::ok(vaddr + paddr_index * PAGE_SIZE);
      }
    }
  }

  defer.cancel();
  return zx::error(ZX_ERR_NOT_FOUND);
}

}  // namespace ufs_mock_device
}  // namespace ufs
