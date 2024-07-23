// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <vm/evictor.h>
#include <vm/stack_owned_loaned_pages_interval.h>

#include "test_helper.h"

namespace vm_unittest {

// Custom pmm node to link with the evictor under test. Facilitates verifying the free count which
// is not possible with the global pmm node.
class TestPmmNode {
 public:
  explicit TestPmmNode(bool discardable)
      : evictor_(&node_, pmm_page_queues(),
                 discardable ? Evictor::kEvictDiscardable : Evictor::kEvictPagerBacked) {
    evictor_.EnableEviction(true);
  }

  ~TestPmmNode() {
    // Pages that were evicted are being held in |node_|'s free list.
    // Return them to the global pmm node before exiting.
    DecrementFreePages(node_.CountFreePages());
    ASSERT(node_.CountFreePages() == 0);
  }

  // Reduce free pages in |node_| by |num_pages|.
  void DecrementFreePages(uint64_t num_pages) {
    uint64_t free_count = node_.CountFreePages();
    if (free_count < num_pages) {
      num_pages = free_count;
    }
    list_node list = LIST_INITIAL_VALUE(list);
    zx_status_t status = node_.AllocPages(num_pages, 0, &list);
    ASSERT(status == ZX_OK);

    // Return these pages to the global pmm. Our goal is to just reduce the free count of |node_|,
    // we do not intend to use the allocated pages for anything.
    vm_page_t* page;
    list_for_every_entry (&list, page, vm_page_t, queue_node) {
      page->set_state(vm_page_state::ALLOC);
    }
    pmm_free(&list);
  }

  Evictor::EvictionTarget GetOneShotEvictionTarget() const {
    return evictor_.DebugGetOneShotEvictionTarget();
  }

  uint64_t FreePages() const { return node_.CountFreePages(); }

  Evictor* evictor() { return &evictor_; }

 private:
  PmmNode node_;
  Evictor evictor_;
};

// Test that a one shot eviction target can be set as expected.
static bool evictor_set_target_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;
  TestPmmNode node(false);

  auto expected = Evictor::EvictionTarget{
      .pending = static_cast<bool>(rand() % 2),
      .free_pages_target = static_cast<uint64_t>(rand()),
      .min_pages_to_free = static_cast<uint64_t>(rand()),
      .level =
          (rand() % 2) ? Evictor::EvictionLevel::IncludeNewest : Evictor::EvictionLevel::OnlyOldest,
  };

  node.evictor()->SetOneShotEvictionTarget(expected);

  auto actual = node.GetOneShotEvictionTarget();

  ASSERT_EQ(actual.pending, expected.pending);
  ASSERT_EQ(actual.free_pages_target, expected.free_pages_target);
  ASSERT_EQ(actual.min_pages_to_free, expected.min_pages_to_free);
  ASSERT_EQ(actual.level, expected.level);

  END_TEST;
}

// Test that multiple one shot eviction targets can be combined as expected.
static bool evictor_combine_targets_test() {
  BEGIN_TEST;

  AutoVmScannerDisable scanner_disable;
  TestPmmNode node(false);

  static constexpr int kNumTargets = 5;
  Evictor::EvictionTarget targets[kNumTargets];

  for (auto& target : targets) {
    target = Evictor::EvictionTarget{
        .pending = true,
        .free_pages_target = static_cast<uint64_t>(rand() % 1000),
        .min_pages_to_free = static_cast<uint64_t>(rand() % 1000),
        .level = Evictor::EvictionLevel::IncludeNewest,
    };
    node.evictor()->CombineOneShotEvictionTarget(target);
  }

  Evictor::EvictionTarget expected = {};
  for (auto& target : targets) {
    expected.pending = expected.pending || target.pending;
    expected.level = ktl::max(expected.level, target.level);
    expected.min_pages_to_free += target.min_pages_to_free;
    expected.free_pages_target = ktl::max(expected.free_pages_target, target.free_pages_target);
  }

  auto actual = node.GetOneShotEvictionTarget();

  ASSERT_EQ(actual.pending, expected.pending);
  ASSERT_EQ(actual.free_pages_target, expected.free_pages_target);
  ASSERT_EQ(actual.min_pages_to_free, expected.min_pages_to_free);
  ASSERT_EQ(actual.level, expected.level);

  END_TEST;
}

// Helper to create a pager backed vmo and commit all its pages.
static zx_status_t create_precommitted_pager_backed_vmo(uint64_t size,
                                                        fbl::RefPtr<VmObjectPaged>* vmo_out,
                                                        vm_page_t** out_pages = nullptr) {
  // The size should be page aligned for TakePages and SupplyPages to work.
  if (size % PAGE_SIZE) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  fbl::RefPtr<StubPageProvider> pager = fbl::MakeRefCountedChecked<StubPageProvider>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  fbl::RefPtr<PageSource> src = fbl::MakeRefCountedChecked<PageSource>(&ac, ktl::move(pager));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateExternal(ktl::move(src), 0u, size, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  // Create an aux VMO to transfer pages into the pager-backed vmo.
  fbl::RefPtr<VmObjectPaged> aux_vmo;
  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, size, &aux_vmo);
  if (status != ZX_OK) {
    return status;
  }

  status = aux_vmo->CommitRange(0, size);
  if (status != ZX_OK) {
    return status;
  }

  VmPageSpliceList page_list;
  status = aux_vmo->TakePages(0, size, &page_list);
  if (status != ZX_OK) {
    return status;
  }

  status = vmo->SupplyPages(0, size, &page_list, SupplyOptions::PagerSupply);
  if (status != ZX_OK) {
    return status;
  }

  // Pin the pages momentarily to force the pages to be non-loaned pages.  This allows us to be more
  // strict with asserts that verify how many non-loaned pages are evicted.  Loaned pages can also
  // be evicted along the way to evicting non-loaned pages, but only non-loaned pages count as fully
  // free.
  ASSERT(ZX_OK == vmo->CommitRangePinned(0, size, false));
  vmo->Unpin(0, size);

  // Get the pages after the pin, so that we find non-loaned pages.
  if (out_pages) {
    for (uint64_t i = 0; i < size; i += PAGE_SIZE) {
      status = vmo->GetPage(i, 0, nullptr, nullptr, &out_pages[i / PAGE_SIZE], nullptr);
      if (status != ZX_OK) {
        return status;
      }
    }
  }

  *vmo_out = ktl::move(vmo);
  return ZX_OK;
}

// Test that the evictor can evict from pager backed vmos as expected.
static bool evictor_pager_backed_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager backed vmo to evict pages from.
  fbl::RefPtr<VmObjectPaged> vmo;
  static constexpr size_t kNumPages = 22;
  ASSERT_EQ(ZX_OK, create_precommitted_pager_backed_vmo(kNumPages * PAGE_SIZE, &vmo));

  // Promote the pages for eviction.
  vmo->HintRange(0, kNumPages * PAGE_SIZE, VmObject::EvictionHint::DontNeed);

  TestPmmNode node(false);

  auto target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 20,
      .min_pages_to_free = 10,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  // The node starts off with zero pages.
  uint64_t free_count = node.FreePages();
  EXPECT_EQ(free_count, 0u);

  node.evictor()->SetOneShotEvictionTarget(target);
  auto counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages were evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Free pages target was greater than min pages target. So precisely free pages target must have
  // been evicted.
  EXPECT_EQ(counts.pager_backed, target.free_pages_target);
  EXPECT_GE(counts.pager_backed, target.min_pages_to_free);
  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  free_count = node.FreePages();
  EXPECT_EQ(free_count, target.free_pages_target);
  EXPECT_GE(free_count, target.min_pages_to_free);

  // Re-initialize the vmo and try again with a different target.
  vmo.reset();
  ASSERT_EQ(ZX_OK, create_precommitted_pager_backed_vmo(kNumPages * PAGE_SIZE, &vmo));
  // Promote the pages for eviction.
  vmo->HintRange(0, kNumPages * PAGE_SIZE, VmObject::EvictionHint::DontNeed);

  target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 10,
      .min_pages_to_free = 20,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  node.evictor()->SetOneShotEvictionTarget(target);
  counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages were evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Min pages target was greater than free pages target. So precisely min pages target must have
  // been evicted.
  EXPECT_EQ(counts.pager_backed, target.min_pages_to_free);
  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  EXPECT_GE(node.FreePages(), target.free_pages_target);
  EXPECT_EQ(node.FreePages(), free_count + target.min_pages_to_free);

  END_TEST;
}

// Helper to create a fully committed discardable vmo, which is unlocked and can be evicted.
static zx_status_t create_committed_unlocked_discardable_vmo(uint64_t size,
                                                             fbl::RefPtr<VmObjectPaged>* vmo_out) {
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kDiscardable, size, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  // Lock and commit the vmo.
  status = vmo->TryLockRange(0, size);
  if (status != ZX_OK) {
    return status;
  }
  status = vmo->CommitRange(0, size);
  if (status != ZX_OK) {
    return status;
  }

  // Unlock the vmo and move it out of the active queues so that it can be discarded.
  status = vmo->UnlockRange(0, size);
  if (status != ZX_OK) {
    return status;
  }
  for (size_t i = 0; i < PageQueues::kNumActiveQueues; i++) {
    pmm_page_queues()->RotateReclaimQueues();
  }

  *vmo_out = ktl::move(vmo);
  return ZX_OK;
}

// Test that the evictor can discard from discardable vmos as expected.
static bool evictor_discardable_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a discardable vmo.
  fbl::RefPtr<VmObjectPaged> vmo;
  static constexpr size_t kNumPages = 22;
  ASSERT_EQ(ZX_OK, create_committed_unlocked_discardable_vmo(kNumPages * PAGE_SIZE, &vmo));

  TestPmmNode node(true);

  auto target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 20,
      .min_pages_to_free = 10,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  // The node starts off with zero pages.
  uint64_t free_count = node.FreePages();
  EXPECT_EQ(free_count, 0u);

  node.evictor()->SetOneShotEvictionTarget(target);
  auto counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No pager backed pages were evicted.
  EXPECT_EQ(counts.pager_backed, 0u);
  // Free pages target was greater than min pages target. So precisely free pages target must have
  // been evicted. However, a discardable vmo can only be discarded in its entirety, so we can't
  // check for equality with free pages target. We can't check for equality with |kNumPages| either
  // as it is possible (albeit unlikely) that a discardable vmo other than the one we created
  // here was discarded, since we're discarding from the global list of discardable vmos. In the
  // future (if and) when vmos are PMM node aware, we will be able to control this better by
  // creating a vmo backed by the test node.
  EXPECT_GE(counts.discardable, target.free_pages_target);
  EXPECT_GE(counts.discardable, target.min_pages_to_free);
  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  free_count = node.FreePages();
  EXPECT_GE(free_count, target.free_pages_target);
  EXPECT_GE(free_count, target.min_pages_to_free);

  // Re-initialize the vmo and try again with a different target.
  vmo.reset();
  ASSERT_EQ(ZX_OK, create_committed_unlocked_discardable_vmo(kNumPages * PAGE_SIZE, &vmo));

  target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 10,
      .min_pages_to_free = 20,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  node.evictor()->SetOneShotEvictionTarget(target);
  counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No pager backed pages were evicted.
  EXPECT_EQ(counts.pager_backed, 0u);
  // Min pages target was greater than free pages target. So precisely min pages target must have
  // been evicted. However, a discardable vmo can only be discarded in its entirety, so we can't
  // check for equality with free pages target. We can't check for equality with |kNumPages| either
  // as it is possible (albeit unlikely) that a discardable vmo other than the one we created
  // here was discarded, since we're discarding from the global list of discardable vmos. In the
  // future (if and) when vmos are PMM node aware, we will be able to control this better by
  // creating a vmo backed by the test node.
  EXPECT_GE(counts.discardable, target.min_pages_to_free);
  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  EXPECT_GE(node.FreePages(), target.free_pages_target);
  EXPECT_GE(node.FreePages(), free_count + target.min_pages_to_free);

  END_TEST;
}

// Test that eviction meets the required free and min target as expected.
static bool evictor_free_target_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager backed vmo to evict pages from.
  fbl::RefPtr<VmObjectPaged> vmo;
  static constexpr size_t kNumPages = 111;
  ASSERT_EQ(ZX_OK, create_precommitted_pager_backed_vmo(kNumPages * PAGE_SIZE, &vmo));

  // Promote the pages for eviction.
  vmo->HintRange(0, kNumPages * PAGE_SIZE, VmObject::EvictionHint::DontNeed);

  // Only evict from pager backed vmos.
  TestPmmNode node(false);

  auto target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 20,
      .min_pages_to_free = 0,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  // The node starts off with zero pages.
  uint64_t free_count = node.FreePages();
  EXPECT_EQ(free_count, 0u);

  node.evictor()->SetOneShotEvictionTarget(target);
  auto counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages were evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Free pages target was greater than min pages target. So precisely free pages target must have
  // been evicted.
  EXPECT_EQ(counts.pager_backed, target.free_pages_target);
  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  free_count = node.FreePages();
  EXPECT_EQ(free_count, target.free_pages_target);
  EXPECT_GE(free_count, target.min_pages_to_free);

  // Evict again with the same target.
  node.evictor()->SetOneShotEvictionTarget(target);
  counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No new pages should have been evicted, as the free target was already met with the previous
  // round of eviction, and no minimum pages were requested to be evicted.
  EXPECT_EQ(counts.discardable, 0u);
  EXPECT_EQ(counts.pager_backed, 0u);
  EXPECT_EQ(node.FreePages(), free_count);

  // Evict again with a higher free memory target. No min pages target.
  uint64_t delta_pages = 10;
  target.free_pages_target += delta_pages;
  target.min_pages_to_free = 0;
  node.evictor()->SetOneShotEvictionTarget(target);
  counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Exactly delta_pages evicted.
  EXPECT_EQ(counts.pager_backed, delta_pages);
  EXPECT_GE(counts.pager_backed, target.min_pages_to_free);
  // Free count increased by delta_pages.
  free_count = node.FreePages();
  EXPECT_EQ(free_count, target.free_pages_target);

  // Evict again with a higher free memory target and also a min pages target.
  target.free_pages_target += delta_pages;
  target.min_pages_to_free = delta_pages;
  node.evictor()->SetOneShotEvictionTarget(target);
  counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Exactly delta_pages evicted.
  EXPECT_EQ(counts.pager_backed, delta_pages);
  EXPECT_GE(counts.pager_backed, target.min_pages_to_free);
  // Free count increased by delta_pages.
  free_count = node.FreePages();
  EXPECT_EQ(free_count, target.free_pages_target);

  // Evict again with the same free target, but request a min number of pages to be freed.
  target.min_pages_to_free = 2;
  node.evictor()->SetOneShotEvictionTarget(target);
  counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Exactly min pages evicted.
  EXPECT_EQ(counts.pager_backed, target.min_pages_to_free);
  // Free count increased by min pages.
  EXPECT_EQ(node.FreePages(), free_count + target.min_pages_to_free);

  END_TEST;
}

// Test that the evictor can evict DontNeed hinted pager backed pages as expected.
static bool evictor_dont_need_pager_backed_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager backed vmo with committed pages.
  fbl::RefPtr<VmObjectPaged> vmo1;
  static constexpr size_t kNumPages = 5;
  ASSERT_EQ(ZX_OK, create_precommitted_pager_backed_vmo(kNumPages * PAGE_SIZE, &vmo1));

  // Promote the pages for eviction. This will put these pages in the DontNeed queue.
  vmo1->HintRange(0, kNumPages * PAGE_SIZE, VmObject::EvictionHint::DontNeed);
  // Now touch these pages, changing the queue stashed in their vm_page_t without actually moving
  // them from the DontNeed queue. The expectation is that the next eviction attempt will fix up the
  // queue for these pages.
  for (size_t i = 0; i < kNumPages; i++) {
    uint8_t data;
    ASSERT_EQ(ZX_OK, vmo1->Read(&data, i * PAGE_SIZE, sizeof(data)));
  }

  // Create another pager backed vmo, which has newer pages compared to the previous one. This will
  // supply the pages below that actually get evicted.
  fbl::RefPtr<VmObjectPaged> vmo2;
  ASSERT_EQ(ZX_OK, create_precommitted_pager_backed_vmo(kNumPages * PAGE_SIZE, &vmo2));

  // Promote the pages for eviction. This will put these pages in the DontNeed queue in LRU order,
  // i.e. they will be considered for eviction only after vmo1's pages.
  vmo2->HintRange(0, kNumPages * PAGE_SIZE, VmObject::EvictionHint::DontNeed);

  TestPmmNode node(false);

  auto target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 5,
      .min_pages_to_free = 5,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  // The node starts off with zero pages.
  uint64_t free_count = node.FreePages();
  EXPECT_EQ(free_count, 0u);

  node.evictor()->SetOneShotEvictionTarget(target);
  auto counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages were evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Free pages target was the same as min pages target. So precisely free pages target must have
  // been evicted.
  EXPECT_EQ(counts.pager_backed, target.free_pages_target);
  EXPECT_GE(counts.pager_backed, target.min_pages_to_free);
  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  free_count = node.FreePages();
  EXPECT_EQ(free_count, target.free_pages_target);
  EXPECT_GE(free_count, target.min_pages_to_free);

  // vmo1 should have no pages evicted from it.
  EXPECT_EQ(kNumPages * PAGE_SIZE, vmo1->GetAttributedMemory().uncompressed_bytes);

  END_TEST;
}

// Tests that evicted pages are removed from the VMO *and* added to the pmm free pool. Regression
// test for https://fxbug.dev/42153484.
static bool evictor_evicted_pages_are_freed_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

  // Create a pager backed vmo with committed pages.
  fbl::RefPtr<VmObjectPaged> vmo;
  static constexpr size_t kNumPages = 5;
  vm_page_t* pages[kNumPages];
  ASSERT_EQ(ZX_OK, create_precommitted_pager_backed_vmo(kNumPages * PAGE_SIZE, &vmo, pages));

  // Verify that the vmo has committed pages.
  EXPECT_EQ(kNumPages * PAGE_SIZE, vmo->GetAttributedMemory().uncompressed_bytes);

  // Rotate page queues a few times so the newly committed pages above are eligible for eviction.
  for (int i = 0; i < 3; i++) {
    pmm_page_queues()->RotateReclaimQueues();
  }

  // Only evict from pager backed vmos.
  TestPmmNode node(false);

  auto target = Evictor::EvictionTarget{
      .pending = true,
      // Ensure that all evictable pages end up evicted, so we can verify that the vmo we created
      // has no pages remaining.
      .free_pages_target = UINT64_MAX,
      .min_pages_to_free = 0,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  // The node starts off with zero pages.
  uint64_t free_count = node.FreePages();
  EXPECT_EQ(free_count, 0u);

  node.evictor()->SetOneShotEvictionTarget(target);
  auto counts = node.evictor()->EvictOneShotFromPreloadedTarget();

  // No discardable pages were evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Evicted pager backed pages should be more than or equal to the vmo's pages. If there were no
  // other evictable pages, we should at least have been able to evict from the vmo we created.
  EXPECT_GE(counts.pager_backed, kNumPages);
  EXPECT_GE(counts.pager_backed, target.min_pages_to_free);

  // The node has the desired number of free pages now, and a minimum of min pages have been freed.
  free_count = node.FreePages();
  EXPECT_GE(free_count, kNumPages);
  EXPECT_GE(free_count, target.min_pages_to_free);

  // All the evicted pages should have ended up in the node's free list. Pages that were evicted in
  // this test is the only way we can end up with free pages in this node. This verifies that
  // pages evicted from pager-backed vmos are freed.
  EXPECT_EQ(free_count, counts.pager_backed);

  // Verify that the vmo has no committed pages remaining. Evicted pages are removed from the vmo.
  EXPECT_TRUE(VmObject::AttributionCounts{} == vmo->GetAttributedMemory());

  // Verify free state for each page.
  for (auto page : pages) {
    EXPECT_TRUE(page->is_free());
  }

  END_TEST;
}

UNITTEST_START_TESTCASE(evictor_tests)
VM_UNITTEST(evictor_set_target_test)
VM_UNITTEST(evictor_combine_targets_test)
VM_UNITTEST(evictor_pager_backed_test)
VM_UNITTEST(evictor_discardable_test)
VM_UNITTEST(evictor_free_target_test)
VM_UNITTEST(evictor_dont_need_pager_backed_test)
VM_UNITTEST(evictor_evicted_pages_are_freed_test)
UNITTEST_END_TESTCASE(evictor_tests, "evictor", "Evictor tests")

}  // namespace vm_unittest
