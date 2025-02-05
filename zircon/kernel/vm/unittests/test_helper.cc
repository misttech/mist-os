// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "test_helper.h"

#include <lib/fit/defer.h>

namespace vm_unittest {

// Helper function to allocate memory in a user address space.
zx_status_t AllocUser(VmAspace* aspace, const char* name, size_t size, user_inout_ptr<void>* ptr) {
  ASSERT(aspace->is_user());

  size = ROUNDUP(size, PAGE_SIZE);
  if (size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, size, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  vmo->set_name(name, strlen(name));
  static constexpr const uint kArchFlags = kArchRwFlags | ARCH_MMU_FLAG_PERM_USER;
  auto mapping = aspace->RootVmar()->CreateVmMapping(0, size, 0, 0, vmo, 0, kArchFlags, name);
  if (mapping.is_error()) {
    return mapping.status_value();
  }

  *ptr = make_user_inout_ptr(reinterpret_cast<void*>(mapping->base));
  return ZX_OK;
}

zx_status_t make_uncommitted_pager_vmo(size_t num_pages, bool trap_dirty, bool resizable,
                                       fbl::RefPtr<VmObjectPaged>* out_vmo) {
  fbl::AllocChecker ac;
  fbl::RefPtr<StubPageProvider> pager =
      fbl::MakeRefCountedChecked<StubPageProvider>(&ac, trap_dirty);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  fbl::RefPtr<PageSource> src = fbl::MakeRefCountedChecked<PageSource>(&ac, ktl::move(pager));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateExternal(
      ktl::move(src), resizable ? VmObjectPaged::kResizable : 0, num_pages * PAGE_SIZE, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  *out_vmo = ktl::move(vmo);
  return ZX_OK;
}

zx_status_t make_committed_pager_vmo(size_t num_pages, bool trap_dirty, bool resizable,
                                     vm_page_t** out_pages, fbl::RefPtr<VmObjectPaged>* out_vmo) {
  // Disable the scanner so we can safely submit our aux vmo and query pages without eviction
  // happening.
  AutoVmScannerDisable scanner_disable;

  // Create a pager backed VMO and jump through some hoops to pre-fill pages for it so we do not
  // actually take any page faults.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = make_uncommitted_pager_vmo(num_pages, trap_dirty, resizable, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<VmObjectPaged> aux_vmo;
  status = VmObjectPaged::Create(0, 0, num_pages * PAGE_SIZE, &aux_vmo);
  if (status != ZX_OK) {
    return status;
  }

  status = aux_vmo->CommitRange(0, num_pages * PAGE_SIZE);
  if (status != ZX_OK) {
    return status;
  }

  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  VmPageSpliceList splice_list;
  status = aux_vmo->TakePages(0, num_pages * PAGE_SIZE, &splice_list);
  if (status != ZX_OK) {
    return status;
  }

  status = vmo->SupplyPages(0, num_pages * PAGE_SIZE, &splice_list, SupplyOptions::PagerSupply);
  if (status != ZX_OK) {
    return status;
  }

  for (uint64_t i = 0; i < num_pages; i++) {
    status = vmo->GetPage(i * PAGE_SIZE, 0, nullptr, nullptr, &out_pages[i], nullptr);
    if (status != ZX_OK) {
      return status;
    }
  }

  *out_vmo = ktl::move(vmo);
  return ZX_OK;
}

uint32_t test_rand(uint32_t seed) { return (seed = seed * 1664525 + 1013904223); }

// fill a region of memory with a pattern based on the address of the region
void fill_region(uintptr_t seed, void* _ptr, size_t len) {
  uint32_t* ptr = (uint32_t*)_ptr;

  ASSERT(IS_ALIGNED((uintptr_t)ptr, 4));

  uint32_t val = (uint32_t)seed;
  val ^= (uint32_t)(seed >> 32);
  for (size_t i = 0; i < len / 4; i++) {
    ptr[i] = val;

    val = test_rand(val);
  }
}

// just like |fill_region|, but for user memory
void fill_region_user(uintptr_t seed, user_inout_ptr<void> _ptr, size_t len) {
  user_inout_ptr<uint32_t> ptr = _ptr.reinterpret<uint32_t>();

  ASSERT(IS_ALIGNED(ptr.get(), 4));

  uint32_t val = (uint32_t)seed;
  val ^= (uint32_t)(seed >> 32);
  for (size_t i = 0; i < len / 4; i++) {
    zx_status_t status = ptr.element_offset(i).copy_to_user(val);
    ASSERT(status == ZX_OK);

    val = test_rand(val);
  }
}

// test a region of memory against a known pattern
bool test_region(uintptr_t seed, void* _ptr, size_t len) {
  uint32_t* ptr = (uint32_t*)_ptr;

  ASSERT(IS_ALIGNED((uintptr_t)ptr, 4));

  uint32_t val = (uint32_t)seed;
  val ^= (uint32_t)(seed >> 32);
  for (size_t i = 0; i < len / 4; i++) {
    if (ptr[i] != val) {
      unittest_printf("value at %p (%zu) is incorrect: 0x%x vs 0x%x\n", &ptr[i], i, ptr[i], val);
      return false;
    }

    val = test_rand(val);
  }

  return true;
}

// just like |test_region|, but for user memory
bool test_region_user(uintptr_t seed, user_inout_ptr<void> _ptr, size_t len) {
  user_inout_ptr<uint32_t> ptr = _ptr.reinterpret<uint32_t>();

  ASSERT(IS_ALIGNED(ptr.get(), 4));

  uint32_t val = (uint32_t)seed;
  val ^= (uint32_t)(seed >> 32);
  for (size_t i = 0; i < len / 4; i++) {
    auto p = ptr.element_offset(i);
    uint32_t actual;
    zx_status_t status = p.copy_from_user(&actual);
    ASSERT(status == ZX_OK);
    if (actual != val) {
      unittest_printf("value at %p (%zu) is incorrect: 0x%x vs 0x%x\n", p.get(), i, actual, val);
      return false;
    }

    val = test_rand(val);
  }

  return true;
}

bool fill_and_test(void* ptr, size_t len) {
  BEGIN_TEST;

  // fill it with a pattern
  fill_region((uintptr_t)ptr, ptr, len);

  // test that the pattern is read back properly
  auto result = test_region((uintptr_t)ptr, ptr, len);
  EXPECT_TRUE(result, "testing region for corruption");

  END_TEST;
}

// just like |fill_and_test|, but for user memory
bool fill_and_test_user(user_inout_ptr<void> ptr, size_t len) {
  BEGIN_TEST;

  const auto seed = reinterpret_cast<uintptr_t>(ptr.get());

  // fill it with a pattern
  fill_region_user(seed, ptr, len);

  // test that the pattern is read back properly
  auto result = test_region_user(seed, ptr, len);
  EXPECT_TRUE(result, "testing region for corruption");

  END_TEST;
}

// Helper function used by vmo_mapping_page_fault_optimisation_test.
// Given a mapping, check that a run of consecutive pages are mapped (indicated by
// expected_mapped_page_count) and that remaining pages are unmapped.
bool verify_mapped_page_range(vaddr_t base, size_t mapping_size,
                              size_t expected_mapped_page_count) {
  BEGIN_TEST;

  // All pages up to expected_mapped_page_count should have been faulted.
  paddr_t paddr_writable;
  uint mmu_flags;
  uint64_t offset = 0;
  for (; offset < expected_mapped_page_count * PAGE_SIZE; offset += PAGE_SIZE) {
    ASSERT_OK(Thread::Current::Get()->active_aspace()->arch_aspace().Query(
        base + offset, &paddr_writable, &mmu_flags));
    EXPECT_TRUE(mmu_flags & ARCH_MMU_FLAG_PERM_READ);
  }

  // Remaining pages should not have been faulted in.
  for (; offset < mapping_size; offset += PAGE_SIZE) {
    ASSERT_EQ(Thread::Current::Get()->active_aspace()->arch_aspace().Query(
                  base + offset, &paddr_writable, &mmu_flags),
              ZX_ERR_NOT_FOUND);
  }

  END_TEST;
}

VmObject::AttributionCounts make_private_attribution_counts(uint64_t uncompressed,
                                                            uint64_t compressed) {
  return vm::AttributionCounts{
      .uncompressed_bytes = uncompressed,
      .compressed_bytes = compressed,
      .private_uncompressed_bytes = uncompressed,
      .private_compressed_bytes = compressed,
      .scaled_uncompressed_bytes = vm::FractionalBytes(uncompressed),
      .scaled_compressed_bytes = vm::FractionalBytes(compressed),
  };
}

}  // namespace vm_unittest
