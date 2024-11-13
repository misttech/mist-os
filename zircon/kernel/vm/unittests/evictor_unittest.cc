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
      : evictor_(
            [this](VmCompressor* compression_instance, Evictor::EvictionLevel eviction_level) {
              return this->TestReclaim(compression_instance, eviction_level);
            },
            [this]() { return this->FreePages(); }),
        discardable_(discardable) {
    evictor_.EnableEviction(true);
  }

  ~TestPmmNode() = default;
  // Reduce free pages in |node_| by |num_pages|.
  void DecrementFreePages(uint64_t num_pages) { free_pages_ -= ktl::min(num_pages, free_pages_); }

  void IncrementFreePages(uint64_t num_pages) { free_pages_ += num_pages; }

  Evictor::EvictionTarget GetEvictionTarget() const { return evictor_.DebugGetEvictionTarget(); }

  uint64_t FreePages() const { return free_pages_; }

  Evictor* evictor() { return &evictor_; }

 private:
  ktl::optional<Evictor::EvictedPageCounts> TestReclaim(VmCompressor* compression_instance,
                                                        Evictor::EvictionLevel eviction_level) {
    if (discardable_) {
      // Discardable VMOs get freed in their entirety, which could be any amount of pages. Claiming
      // 10 here is a bit arbitrary, and could be made configurable if/when there are some tests
      // that need it.
      IncrementFreePages(10);
      return Evictor::EvictedPageCounts{
          .discardable = 10,
      };
    }
    IncrementFreePages(1);
    return Evictor::EvictedPageCounts{
        .pager_backed = 1,
    };
  }

  uint64_t free_pages_ = 0;
  Evictor evictor_;
  bool discardable_;
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

  node.evictor()->SetEvictionTarget(expected);

  auto actual = node.GetEvictionTarget();

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
    node.evictor()->CombineEvictionTarget(target);
  }

  Evictor::EvictionTarget expected = {};
  for (auto& target : targets) {
    expected.pending = expected.pending || target.pending;
    expected.level = ktl::max(expected.level, target.level);
    expected.min_pages_to_free += target.min_pages_to_free;
    expected.free_pages_target = ktl::max(expected.free_pages_target, target.free_pages_target);
  }

  auto actual = node.GetEvictionTarget();

  ASSERT_EQ(actual.pending, expected.pending);
  ASSERT_EQ(actual.free_pages_target, expected.free_pages_target);
  ASSERT_EQ(actual.min_pages_to_free, expected.min_pages_to_free);
  ASSERT_EQ(actual.level, expected.level);

  END_TEST;
}

// Test that the evictor can evict from pager backed vmos as expected.
static bool evictor_pager_backed_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

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

  node.evictor()->SetEvictionTarget(target);
  auto counts = node.evictor()->EvictFromPreloadedTarget();

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

  target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 10,
      .min_pages_to_free = 20,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  node.evictor()->SetEvictionTarget(target);
  counts = node.evictor()->EvictFromPreloadedTarget();

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

// Test that the evictor can discard from discardable vmos as expected.
static bool evictor_discardable_test() {
  BEGIN_TEST;
  AutoVmScannerDisable scanner_disable;

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

  node.evictor()->SetEvictionTarget(target);
  auto counts = node.evictor()->EvictFromPreloadedTarget();

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

  target = Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = 10,
      .min_pages_to_free = 20,
      .level = Evictor::EvictionLevel::IncludeNewest,
  };

  node.evictor()->SetEvictionTarget(target);
  counts = node.evictor()->EvictFromPreloadedTarget();

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

  node.evictor()->SetEvictionTarget(target);
  auto counts = node.evictor()->EvictFromPreloadedTarget();

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
  node.evictor()->SetEvictionTarget(target);
  counts = node.evictor()->EvictFromPreloadedTarget();

  // No new pages should have been evicted, as the free target was already met with the previous
  // round of eviction, and no minimum pages were requested to be evicted.
  EXPECT_EQ(counts.discardable, 0u);
  EXPECT_EQ(counts.pager_backed, 0u);
  EXPECT_EQ(node.FreePages(), free_count);

  // Evict again with a higher free memory target. No min pages target.
  uint64_t delta_pages = 10;
  target.free_pages_target += delta_pages;
  target.min_pages_to_free = 0;
  node.evictor()->SetEvictionTarget(target);
  counts = node.evictor()->EvictFromPreloadedTarget();

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
  node.evictor()->SetEvictionTarget(target);
  counts = node.evictor()->EvictFromPreloadedTarget();

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
  node.evictor()->SetEvictionTarget(target);
  counts = node.evictor()->EvictFromPreloadedTarget();

  // No discardable pages evicted.
  EXPECT_EQ(counts.discardable, 0u);
  // Exactly min pages evicted.
  EXPECT_EQ(counts.pager_backed, target.min_pages_to_free);
  // Free count increased by min pages.
  EXPECT_EQ(node.FreePages(), free_count + target.min_pages_to_free);

  END_TEST;
}

UNITTEST_START_TESTCASE(evictor_tests)
VM_UNITTEST(evictor_set_target_test)
VM_UNITTEST(evictor_combine_targets_test)
VM_UNITTEST(evictor_pager_backed_test)
VM_UNITTEST(evictor_discardable_test)
VM_UNITTEST(evictor_free_target_test)
UNITTEST_END_TESTCASE(evictor_tests, "evictor", "Evictor tests")

}  // namespace vm_unittest
