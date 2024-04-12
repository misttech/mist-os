// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_IOMMU_INTEL_SECOND_LEVEL_PT_H_
#define ZIRCON_KERNEL_DEV_IOMMU_INTEL_SECOND_LEVEL_PT_H_

#include <arch/x86/page_tables/page_tables.h>
#include <vm/pmm.h>

#include "hw.h"

namespace intel_iommu {

class DeviceContext;
class IommuImpl;

// Implementation of second-level page tables used by VT-d
class SecondLevelPageTable final : public X86PageTableImpl<SecondLevelPageTable> {
 public:
  SecondLevelPageTable(IommuImpl* iommu, DeviceContext* parent);
  ~SecondLevelPageTable();

  zx_status_t Init(PageTableLevel top_level);
  void Destroy();

  PageTableLevel top_level() { return top_level_; }
  bool allowed_flags(uint flags);
  bool check_paddr(paddr_t paddr);
  bool check_vaddr(vaddr_t vaddr);
  bool supports_page_size(PageTableLevel level);
  IntermediatePtFlags intermediate_flags();
  PtFlags terminal_flags(PageTableLevel level, uint flags);
  PtFlags split_flags(PageTableLevel level, PtFlags flags);
  void TlbInvalidate(const PendingTlbInvalidation* pending);
  uint pt_flags_to_mmu_flags(PtFlags flags, PageTableLevel level);
  bool needs_cache_flushes() { return needs_flushes_; }

  IommuImpl* iommu_;
  DeviceContext* parent_;

  PageTableLevel top_level_;
  bool needs_flushes_;
  bool supports_2mb_;
  bool supports_1gb_;

  vaddr_t valid_vaddr_mask_;
  bool initialized_;
};

}  // namespace intel_iommu

#endif  // ZIRCON_KERNEL_DEV_IOMMU_INTEL_SECOND_LEVEL_PT_H_
