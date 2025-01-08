// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>

#include "test_helper.h"

namespace vm_unittest {

namespace {

// Helper class for managing a PmmNode with real pages. AllocRange and AllocContiguous are not
// supported by the managed PmmNode object. Only a single instance can exist at a time.
class ManagedPmmNode {
 public:
  static constexpr size_t kNumPages = 64;
  static constexpr size_t kDefaultMemEventLowerBound = kNumPages / 2;
  static constexpr size_t kDefaultShouldWaitLevel = kNumPages / 4;

  static constexpr size_t kDefaultLowMemAlloc =
      ManagedPmmNode::kNumPages - kDefaultShouldWaitLevel + 1;
  static constexpr size_t kDefaultMemEventAlloc =
      ManagedPmmNode::kNumPages - kDefaultMemEventLowerBound + 1;

  ManagedPmmNode() {
    list_node list = LIST_INITIAL_VALUE(list);
    ZX_ASSERT(pmm_alloc_pages(kNumPages, 0, &list) == ZX_OK);
    vm_page_t* page;
    list_for_every_entry (&list, page, vm_page_t, queue_node) {
      // TODO: Prevent this page state from allowing AllocContiguous() to potentially find a run of
      // FREE pages involving some of these pages.
      page->set_state(vm_page_state::FREE);
    }
    node_.AddFreePages(&list);

    // Test node always has checking enabled.
    ASSERT(node_.EnableFreePageFilling(PAGE_SIZE, CheckFailAction::kPanic));
    node_.FillFreePagesAndArm();

    bool result = ResetDefaultMemEvent();
    ASSERT(result);
  }

  ~ManagedPmmNode() {
    list_node list = LIST_INITIAL_VALUE(list);
    zx_status_t status = node_.AllocPages(kNumPages, 0, &list);
    ASSERT(status == ZX_OK);
    vm_page_t* page;
    list_for_every_entry (&list, page, vm_page_t, queue_node) {
      page->set_state(vm_page_state::ALLOC);
    }
    pmm_free(&list);
  }

  bool IsEventSignaled() { return event_.Wait(Deadline::infinite_past()) == ZX_OK; }

  void UnsignalEvent() { event_.Unsignal(); }

  bool ResetDefaultMemEvent() {
    return SetFreeMemorySignal(kDefaultMemEventLowerBound, UINT64_MAX, kDefaultShouldWaitLevel);
  }

  bool SetFreeMemorySignal(uint64_t lower_bound, uint64_t higher_bound, uint64_t delay_pages) {
    return node_.SetFreeMemorySignal(lower_bound, higher_bound, delay_pages, &event_);
  }

  PmmNode& node() { return node_; }

  zx_status_t AllocLoanedPages(size_t count, list_node_t* list) {
    list_node alloc_list = LIST_INITIAL_VALUE(alloc_list);
    for (size_t i = 0; i < count; i++) {
      zx::result<vm_page_t*> result = node_.AllocLoanedPage();
      if (result.is_error()) {
        if (!list_is_empty(&alloc_list)) {
          node_.FreeLoanedList(&alloc_list);
        }
        return result.status_value();
      }
      list_add_tail(&alloc_list, &(*result)->queue_node);
    }
    if (list_is_empty(list)) {
      list_move(&alloc_list, list);
    } else {
      list_splice_after(&alloc_list, list_peek_tail(list));
    }
    return ZX_OK;
  }

 private:
  PmmNode node_;
  Event event_;
};

}  // namespace

// Allocates a single page, translates it to a vm_page_t and frees it.
static bool pmm_smoke_test() {
  BEGIN_TEST;
  paddr_t pa;
  vm_page_t* page;

  zx_status_t status = pmm_alloc_page(0, &page, &pa);
  ASSERT_EQ(ZX_OK, status, "pmm_alloc single page");
  ASSERT_NONNULL(page, "pmm_alloc single page");
  ASSERT_NE(0u, pa, "pmm_alloc single page");

  vm_page_t* page2 = paddr_to_vm_page(pa);
  ASSERT_EQ(page2, page, "paddr_to_vm_page on single page");

  pmm_free_page(page);
  END_TEST;
}

// Allocates one page and frees it.
static bool pmm_alloc_contiguous_one_test() {
  BEGIN_TEST;
  list_node list = LIST_INITIAL_VALUE(list);
  paddr_t pa;
  size_t count = 1U;
  zx_status_t status = pmm_alloc_contiguous(count, 0, PAGE_SIZE_SHIFT, &pa, &list);
  ASSERT_EQ(ZX_OK, status, "pmm_alloc_contiguous returned failure\n");
  ASSERT_EQ(count, list_length(&list), "pmm_alloc_contiguous list size is wrong");
  ASSERT_NONNULL(paddr_to_physmap(pa));
  pmm_free(&list);
  END_TEST;
}

// Allocates more than one page and frees them.
static bool pmm_node_multi_alloc_test() {
  BEGIN_TEST;
  ManagedPmmNode node;
  static constexpr size_t alloc_count = ManagedPmmNode::kNumPages / 2;
  list_node list = LIST_INITIAL_VALUE(list);

  zx_status_t status = node.node().AllocPages(alloc_count, 0, &list);
  EXPECT_EQ(ZX_OK, status, "pmm_alloc_pages a few pages");
  EXPECT_EQ(alloc_count, list_length(&list), "pmm_alloc_pages a few pages list count");

  status = node.node().AllocPages(alloc_count, 0, &list);
  EXPECT_EQ(ZX_OK, status, "pmm_alloc_pages a few pages");
  EXPECT_EQ(2 * alloc_count, list_length(&list), "pmm_alloc_pages a few pages list count");

  node.node().FreeList(&list);
  END_TEST;
}

// Allocates one page from the bulk allocation api.
static bool pmm_node_singlton_list_test() {
  BEGIN_TEST;
  ManagedPmmNode node;
  list_node list = LIST_INITIAL_VALUE(list);

  zx_status_t status = node.node().AllocPages(1, 0, &list);
  EXPECT_EQ(ZX_OK, status, "pmm_alloc_pages a few pages");
  EXPECT_EQ(1ul, list_length(&list), "pmm_alloc_pages a few pages list count");

  node.node().FreeList(&list);
  END_TEST;
}

// Loans pages back to the PmmNode, allocates them as usable pages while loaned, cancels that loan,
// reclaims the pages via "churn" (to FREE), ends the loan.
static bool pmm_node_loan_borrow_cancel_reclaim_end() {
  BEGIN_TEST;

  // Required to stack-own loaned pages.  We don't care about minimizing the duration of this
  // interval for this test.
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  ManagedPmmNode node;

  bool was_loaning_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
  bool was_borrowing_enabled =
      pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
  pmm_physical_page_borrowing_config()->set_loaning_enabled(true);
  pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(true);
  auto cleanup = fit::defer([was_loaning_enabled, was_borrowing_enabled] {
    pmm_physical_page_borrowing_config()->set_loaning_enabled(was_loaning_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
        was_borrowing_enabled);
  });

  list_node list = LIST_INITIAL_VALUE(list);

  constexpr uint64_t kLoanCount = ManagedPmmNode::kNumPages * 3 / 4;
  constexpr uint64_t kNotLoanCount = ManagedPmmNode::kNumPages - kLoanCount;
  paddr_t paddr[kLoanCount] = {};

  zx_status_t status = node.node().AllocPages(kLoanCount, 0, &list);
  EXPECT_EQ(ZX_OK, status, "pmm_alloc_pages a few pages");
  EXPECT_EQ(kLoanCount, list_length(&list), "pmm_alloc_pages correct # pages");

  uint32_t i = 0;
  vm_page_t* page;
  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    paddr[i] = page->paddr();
    ++i;
  }

  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    EXPECT_FALSE(page->is_loaned());
    EXPECT_FALSE(page->is_loan_cancelled());
  }
  node.node().BeginLoan(&list);
  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    EXPECT_TRUE(page->is_loaned());
    EXPECT_FALSE(page->is_loan_cancelled());
  }

  EXPECT_EQ(kLoanCount, node.node().CountLoanedPages());
  EXPECT_EQ(kNotLoanCount, node.node().CountFreePages());
  EXPECT_EQ(kLoanCount, node.node().CountLoanedFreePages());
  EXPECT_EQ(0u, node.node().CountLoanCancelledPages());
  EXPECT_EQ(0u, node.node().CountLoanedNotFreePages());

  EXPECT_EQ(0u, list_length(&list));
  status = node.AllocLoanedPages(kLoanCount, &list);
  EXPECT_EQ(ZX_OK, status, "pmm_alloc_pages PMM_ALLOC_FLAG_LOANED");
  EXPECT_EQ(kLoanCount, list_length(&list));

  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    for (i = 0; i < kLoanCount; ++i) {
      if (paddr[i] == page->paddr()) {
        break;
      }
    }
    // match found
    EXPECT_NE(kLoanCount, i);
  }

  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    EXPECT_TRUE(page->is_loaned());
    EXPECT_FALSE(page->is_loan_cancelled());
    node.node().CancelLoan(page->paddr(), 1);
    EXPECT_TRUE(page->is_loaned());
    EXPECT_TRUE(page->is_loan_cancelled());
  }

  EXPECT_EQ(kLoanCount, node.node().CountLoanedPages());
  EXPECT_EQ(kNotLoanCount, node.node().CountFreePages());
  EXPECT_EQ(0u, node.node().CountLoanedFreePages());
  EXPECT_EQ(kLoanCount, node.node().CountLoanCancelledPages());
  EXPECT_EQ(kLoanCount, node.node().CountLoanedNotFreePages());

  node.node().FreeLoanedList(&list);

  EXPECT_EQ(kLoanCount, node.node().CountLoanedPages());
  EXPECT_EQ(kNotLoanCount, node.node().CountFreePages());
  EXPECT_EQ(0u, node.node().CountLoanedFreePages());
  EXPECT_EQ(kLoanCount, node.node().CountLoanCancelledPages());
  // Still not free; loan_cancelled means the page can't be allocated.
  EXPECT_EQ(kLoanCount, node.node().CountLoanedNotFreePages());

  EXPECT_EQ(0u, list_length(&list));
  status = node.AllocLoanedPages(kNotLoanCount + 1, &list);
  EXPECT_EQ(ZX_ERR_NO_RESOURCES, status, "try to allocate a loan_cancelled page");

  EXPECT_EQ(0u, list_length(&list));
  status = node.node().AllocPages(kNotLoanCount, PMM_ALLOC_FLAG_ANY, &list);
  EXPECT_EQ(ZX_OK, status, "allocate all the not-loaned pages");

  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    EXPECT_FALSE(page->is_loaned());
    for (i = 0; i < kLoanCount; ++i) {
      if (paddr[i] == page->paddr()) {
        break;
      }
    }
    // match not found
    EXPECT_EQ(kLoanCount, i);
  }

  node.node().FreeList(&list);

  EXPECT_EQ(0u, list_length(&list));
  list_node tmp_list = LIST_INITIAL_VALUE(tmp_list);
  for (uint32_t j = 0; j < kLoanCount; ++j) {
    EXPECT_EQ(0u, list_length(&tmp_list));
    node.node().EndLoan(paddr[j], 1, &tmp_list);
    EXPECT_EQ(1u, list_length(&tmp_list));
    page = list_remove_head_type(&tmp_list, vm_page, queue_node);
    EXPECT_EQ(paddr[j], page->paddr());
    EXPECT_FALSE(page->is_loaned());
    EXPECT_FALSE(page->is_loan_cancelled());
    list_add_tail(&list, &page->queue_node);
  }

  node.node().FreeList(&list);

  EXPECT_EQ(0u, node.node().CountLoanedPages());
  EXPECT_EQ(ManagedPmmNode::kNumPages, node.node().CountFreePages());
  EXPECT_EQ(0u, node.node().CountLoanedFreePages());
  EXPECT_EQ(0u, node.node().CountLoanCancelledPages());
  EXPECT_EQ(0u, node.node().CountLoanedNotFreePages());

  EXPECT_EQ(0u, list_length(&list));
  status = node.node().AllocPages(ManagedPmmNode::kNumPages, 0, &list);
  EXPECT_EQ(ZX_OK, status, "allocate all pages");
  EXPECT_EQ(ManagedPmmNode::kNumPages, list_length(&list));

  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    EXPECT_FALSE(page->is_loaned());
    EXPECT_FALSE(page->is_loan_cancelled());
  }

  node.node().FreeList(&list);

  EXPECT_EQ(0u, node.node().CountLoanedPages());
  EXPECT_EQ(ManagedPmmNode::kNumPages, node.node().CountFreePages());
  EXPECT_EQ(0u, node.node().CountLoanedFreePages());
  EXPECT_EQ(0u, node.node().CountLoanCancelledPages());
  EXPECT_EQ(0u, node.node().CountLoanedNotFreePages());

  END_TEST;
}

static bool pmm_node_loan_delete_lender() {
  BEGIN_TEST;

  // Required to stack-own loaned pages.  We don't care about minimizing the duration of this
  // interval for this test.
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  ManagedPmmNode node;

  bool was_loaning_enabled = pmm_physical_page_borrowing_config()->is_loaning_enabled();
  bool was_borrowing_enabled =
      pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled();
  pmm_physical_page_borrowing_config()->set_loaning_enabled(true);
  pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(true);
  auto cleanup = fit::defer([was_loaning_enabled, was_borrowing_enabled] {
    pmm_physical_page_borrowing_config()->set_loaning_enabled(was_loaning_enabled);
    pmm_physical_page_borrowing_config()->set_borrowing_in_supplypages_enabled(
        was_borrowing_enabled);
  });

  // Required to stack-own loaned pages.  We don't care about minimizing the duration of this
  list_node list = LIST_INITIAL_VALUE(list);

  constexpr uint64_t kLoanCount = ManagedPmmNode::kNumPages * 3 / 4;
  paddr_t paddr[kLoanCount] = {};

  // Check that DeleteLender() while loaned pages FREE works.

  zx_status_t status = node.node().AllocPages(kLoanCount, 0, &list);
  EXPECT_EQ(ZX_OK, status, "allocate kLoanCount pages");
  EXPECT_EQ(kLoanCount, list_length(&list));

  uint32_t i = 0;
  vm_page_t* page;
  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    paddr[i] = page->paddr();
    ++i;
  }

  node.node().BeginLoan(&list);

  for (uint32_t j = 0; j < kLoanCount; ++j) {
    node.node().DeleteLender(paddr[j], 1);
  }

  EXPECT_EQ(0u, node.node().CountLoanedPages());
  EXPECT_EQ(ManagedPmmNode::kNumPages, node.node().CountFreePages());
  EXPECT_EQ(0u, node.node().CountLoanedFreePages());
  EXPECT_EQ(0u, node.node().CountLoanCancelledPages());
  EXPECT_EQ(0u, node.node().CountLoanedNotFreePages());

  // Check that DeleteLender() while loaned pages used works.

  EXPECT_EQ(0u, list_length(&list));
  status = node.node().AllocPages(kLoanCount, 0, &list);
  EXPECT_EQ(ZX_OK, status, "allocate kLoanCount pages");
  EXPECT_EQ(kLoanCount, list_length(&list));

  i = 0;
  list_for_every_entry (&list, page, vm_page_t, queue_node) {
    paddr[i] = page->paddr();
    ++i;
  }

  node.node().BeginLoan(&list);

  EXPECT_EQ(0u, list_length(&list));
  status = node.AllocLoanedPages(kLoanCount, &list);
  EXPECT_EQ(ZX_OK, status, "allocate kLoanCount pages");
  EXPECT_EQ(kLoanCount, list_length(&list));

  for (uint32_t j = 0; j < kLoanCount; ++j) {
    node.node().DeleteLender(paddr[j], 1);
  }

  node.node().FreeList(&list);

  EXPECT_EQ(0u, node.node().CountLoanedPages());
  EXPECT_EQ(ManagedPmmNode::kNumPages, node.node().CountFreePages());
  EXPECT_EQ(0u, node.node().CountLoanedFreePages());
  EXPECT_EQ(0u, node.node().CountLoanCancelledPages());
  EXPECT_EQ(0u, node.node().CountLoanedNotFreePages());

  END_TEST;
}

// Allocates too many pages and makes sure it fails nicely.
static bool pmm_node_oversized_alloc_test() {
  BEGIN_TEST;
  ManagedPmmNode node;
  list_node list = LIST_INITIAL_VALUE(list);

  zx_status_t status = node.node().AllocPages(ManagedPmmNode::kNumPages + 1, 0, &list);
  EXPECT_EQ(ZX_ERR_NO_MEMORY, status, "pmm_alloc_pages failed to alloc");
  EXPECT_TRUE(list_is_empty(&list), "pmm_alloc_pages list is empty");

  END_TEST;
}

// Check that free memory events work correctly.
static bool pmm_node_free_mem_event_test() {
  BEGIN_TEST;

  ManagedPmmNode node;

  uint64_t free_count = node.node().CountFreePages();

  // Should initially be some free pages, validate this to allow us to assume the ability to -1.
  ASSERT_GT(free_count, 0u);

  // Setting an event range that does not include the current free count should be invalid.
  EXPECT_FALSE(node.SetFreeMemorySignal(free_count + 1, UINT64_MAX, 0));
  EXPECT_FALSE(node.SetFreeMemorySignal(0, free_count - 1, 0));

  // The range can be inclusive of the current free count.
  EXPECT_TRUE(node.SetFreeMemorySignal(free_count, UINT64_MAX, 0));
  EXPECT_TRUE(node.SetFreeMemorySignal(0, free_count, 0));

  // Reset back to the default event.
  EXPECT_TRUE(node.ResetDefaultMemEvent());

  // Should never have triggered the event up to this point.
  EXPECT_FALSE(node.IsEventSignaled());

  // Allocate all but 1 of the pages to trigger the event.
  list_node list = LIST_INITIAL_VALUE(list);

  for (size_t i = 1; i < ManagedPmmNode::kDefaultMemEventAlloc; i++) {
    zx::result<vm_page_t*> result = node.node().AllocPage(0);
    ASSERT_OK(result.status_value());
    vm_page_t* page = *result;
    list_add_tail(&list, &page->queue_node);
    if (node.IsEventSignaled()) {
      printf("Event signaled at step %zu\n", i);
    }
  }
  // Should not have triggered the event yet.
  EXPECT_FALSE(node.IsEventSignaled());

  // Allocate the last page, this should put us over the limit and set the event.
  {
    zx::result<vm_page_t*> result = node.node().AllocPage(0);
    ASSERT_OK(result.status_value());
    vm_page_t* page = *result;
    list_add_tail(&list, &page->queue_node);
  }
  EXPECT_TRUE(node.IsEventSignaled());
  node.UnsignalEvent();

  // Events are one-shot, and so putting a page back and allocating it again should not re-trigger
  // the event.
  node.node().FreePage(list_remove_head_type(&list, vm_page_t, queue_node));
  {
    zx::result<vm_page_t*> result = node.node().AllocPage(0);
    ASSERT_OK(result.status_value());
    vm_page_t* page = *result;
    list_add_tail(&list, &page->queue_node);
  }
  EXPECT_FALSE(node.IsEventSignaled());

  // Set a new free range that should trip ass we return the pages back.
  EXPECT_TRUE(node.SetFreeMemorySignal(0, ManagedPmmNode::kNumPages - 1, 0));

  // Take one page off the list as our final page.
  vm_page_t* page = list_remove_head_type(&list, vm_page_t, queue_node);

  // Return the rest of the list.
  node.node().FreeList(&list);
  // Event should not have tripped yet.
  EXPECT_FALSE(node.IsEventSignaled());

  // Return the last page, should trip.
  node.node().FreePage(page);
  EXPECT_TRUE(node.IsEventSignaled());

  END_TEST;
}

// Checks sync allocation failure when the node crosses a threshold.
static bool pmm_node_low_mem_alloc_failure_test() {
  BEGIN_TEST;
  ManagedPmmNode node;
  list_node list = LIST_INITIAL_VALUE(list);

  // Put the node in an oom state and make sure allocation fails.
  zx_status_t status = node.node().AllocPages(ManagedPmmNode::kDefaultLowMemAlloc, 0, &list);
  EXPECT_EQ(ZX_OK, status);
  // Should also have been signaled.
  EXPECT_TRUE(node.IsEventSignaled());

  zx::result<vm_page_t*> result = node.node().AllocPage(PMM_ALLOC_FLAG_CAN_WAIT);
  EXPECT_EQ(ZX_ERR_SHOULD_WAIT, result.status_value());

  // Waiting for an allocation should block, although to only try with a very small timeout to not
  // make this test take too long.
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            node.node().WaitTillShouldRetrySingleAlloc(Deadline::after(ZX_MSEC(10))));

  // Free the list.
  node.node().FreeList(&list);

  // Allocations will still be delayed until we reset the trigger.
  result = node.node().AllocPage(PMM_ALLOC_FLAG_CAN_WAIT);
  EXPECT_EQ(result.status_value(), ZX_ERR_SHOULD_WAIT);

  EXPECT_TRUE(node.ResetDefaultMemEvent());

  // Allocations should work again, but the PMM is still allowed to randomly fail requests, so we
  // cannot guarantee that any small finite number of allocation attempts will work.
  // We can check that waiting to retry an allocation completes with no timeout though.
  EXPECT_EQ(ZX_OK, node.node().WaitTillShouldRetrySingleAlloc(Deadline::infinite_past()));

  // Reset the signal.
  node.UnsignalEvent();
  // Set a threshold such that a single allocation should trip into the low mem state.
  EXPECT_TRUE(
      node.SetFreeMemorySignal(ManagedPmmNode::kNumPages, UINT64_MAX, ManagedPmmNode::kNumPages));

  // Signal should not yet be set, and allocations should not be delayed.
  EXPECT_FALSE(node.IsEventSignaled());
  EXPECT_EQ(ZX_OK, node.node().WaitTillShouldRetrySingleAlloc(Deadline::infinite_past()));

  // Allocate a single page and validate that allocations are now delayed.
  ASSERT_OK(node.node().AllocPages(1, 0, &list));
  result = node.node().AllocPage(PMM_ALLOC_FLAG_CAN_WAIT);
  EXPECT_EQ(result.status_value(), ZX_ERR_SHOULD_WAIT);
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            node.node().WaitTillShouldRetrySingleAlloc(Deadline::after(ZX_MSEC(10))));

  node.node().FreeList(&list);

  END_TEST;
}

// Test that deliberately putting into a no alloc state (and back out) works.
static bool pmm_node_explicit_should_wait_test() {
  BEGIN_TEST;

  ManagedPmmNode node;

  // Place the node directly into a state the forbids allocations.
  EXPECT_TRUE(node.SetFreeMemorySignal(0, ManagedPmmNode::kNumPages, UINT64_MAX));

  // Allocations that can wait should be blocked.
  zx::result<vm_page_t*> result = node.node().AllocPage(PMM_ALLOC_FLAG_CAN_WAIT);
  EXPECT_EQ(result.status_value(), ZX_ERR_SHOULD_WAIT);
  // Waiting for an allocation should block, although to only try with a very small timeout to not
  // make this test take too long.
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            node.node().WaitTillShouldRetrySingleAlloc(Deadline::after(ZX_MSEC(10))));

  // A regular allocation should work.
  result = node.node().AllocPage(0);
  ASSERT_OK(result.status_value());
  node.node().FreePage(*result);

  // Changing the delayed threshold should re-enable allocations.
  EXPECT_TRUE(node.ResetDefaultMemEvent());

  EXPECT_EQ(ZX_OK, node.node().WaitTillShouldRetrySingleAlloc(Deadline::infinite_past()));

  END_TEST;
}

static bool pmm_checker_test_with_fill_size(size_t fill_size) {
  BEGIN_TEST;

  PmmChecker checker;

  // Starts off unarmed.
  EXPECT_FALSE(checker.IsArmed());

  // Borrow a real page from the PMM, ask the checker to validate it.  See that because the checker
  // is not armed, |ValidatePattern| still returns true even though the page has no pattern.
  vm_page_t* page;
  EXPECT_EQ(pmm_alloc_page(0, &page), ZX_OK);
  page->set_state(vm_page_state::FREE);
  auto p = static_cast<uint8_t*>(paddr_to_physmap(page->paddr()));
  memset(p, 0, PAGE_SIZE);
  EXPECT_TRUE(checker.ValidatePattern(page));
  checker.AssertPattern(page);

  // Set the fill size and see that |GetFillSize| returns the size.
  checker.SetFillSize(fill_size);
  EXPECT_EQ(fill_size, checker.GetFillSize());

  // Arm the checker and see that |ValidatePattern| returns false.
  checker.Arm();
  EXPECT_TRUE(checker.IsArmed());
  EXPECT_FALSE(checker.ValidatePattern(page));

  // Fill with pattern one less than the fill size and see that it does not pass validation.
  memset(p, 0, fill_size - 1);
  EXPECT_FALSE(checker.ValidatePattern(page));

  // Fill with the full pattern and see that it validates.
  checker.FillPattern(page);
  for (size_t i = 0; i < fill_size; ++i) {
    EXPECT_NE(0, p[i]);
  }
  EXPECT_TRUE(checker.ValidatePattern(page));

  // Corrupt the page after the first |fill_size| bytes and see that the corruption is not detected.
  if (fill_size < PAGE_SIZE) {
    p[fill_size] = 1;
    EXPECT_TRUE(checker.ValidatePattern(page));
  }

  // Corrupt the page within the first |fill_size| bytes and see that the corruption is detected.
  p[fill_size - 1] = 1;
  EXPECT_FALSE(checker.ValidatePattern(page));

  page->set_state(vm_page_state::ALLOC);
  pmm_free_page(page);

  END_TEST;
}

static bool pmm_checker_test() {
  BEGIN_TEST;

  EXPECT_TRUE(pmm_checker_test_with_fill_size(8));
  EXPECT_TRUE(pmm_checker_test_with_fill_size(16));
  EXPECT_TRUE(pmm_checker_test_with_fill_size(512));
  EXPECT_TRUE(pmm_checker_test_with_fill_size(PAGE_SIZE));

  END_TEST;
}

static bool pmm_checker_is_valid_fill_size_test() {
  BEGIN_TEST;

  EXPECT_FALSE(PmmChecker::IsValidFillSize(0));
  EXPECT_FALSE(PmmChecker::IsValidFillSize(7));
  EXPECT_FALSE(PmmChecker::IsValidFillSize(9));
  EXPECT_FALSE(PmmChecker::IsValidFillSize(PAGE_SIZE + 8));
  EXPECT_FALSE(PmmChecker::IsValidFillSize(PAGE_SIZE * 2));

  EXPECT_TRUE(PmmChecker::IsValidFillSize(8));
  EXPECT_TRUE(PmmChecker::IsValidFillSize(16));
  EXPECT_TRUE(PmmChecker::IsValidFillSize(24));
  EXPECT_TRUE(PmmChecker::IsValidFillSize(512));
  EXPECT_TRUE(PmmChecker::IsValidFillSize(PAGE_SIZE));

  END_TEST;
}

static bool pmm_get_arena_info_test() {
  BEGIN_TEST;

  const size_t num_arenas = pmm_num_arenas();
  ASSERT_GT(num_arenas, 0u);

  fbl::AllocChecker ac;
  auto buffer = ktl::unique_ptr<pmm_arena_info_t[]>(new (&ac) pmm_arena_info_t[num_arenas]);
  ASSERT(ac.check());
  const size_t buffer_size = num_arenas * sizeof(pmm_arena_info_t);

  // Not enough room for one.
  zx_status_t status = pmm_get_arena_info(1, 0, buffer.get(), sizeof(pmm_arena_info_t) - 1);
  ASSERT_EQ(status, ZX_ERR_BUFFER_TOO_SMALL);

  // Asking for none.
  status = pmm_get_arena_info(0, 0, buffer.get(), buffer_size);
  ASSERT_EQ(status, ZX_ERR_OUT_OF_RANGE);

  // Asking for more than exist.
  status = pmm_get_arena_info(num_arenas + 1, 0, buffer.get(), buffer_size);
  ASSERT_EQ(status, ZX_ERR_OUT_OF_RANGE);

  // Attempting to skip them all.
  status = pmm_get_arena_info(1, num_arenas, buffer.get(), buffer_size);
  ASSERT_EQ(status, ZX_ERR_OUT_OF_RANGE);

  // Asking for one.
  status = pmm_get_arena_info(1, 0, buffer.get(), buffer_size);
  ASSERT_EQ(status, ZX_OK);

  // Asking for them all.
  status = pmm_get_arena_info(num_arenas, 0, buffer.get(), buffer_size);
  ASSERT_EQ(status, ZX_OK);

  // See they are in ascending order by base.
  paddr_t prev = 0;
  for (unsigned i = 0; i < num_arenas; ++i) {
    if (i == 0) {
      ASSERT_GE(buffer[i].base, prev);
    } else {
      ASSERT_GT(buffer[i].base, prev);
    }
    prev = buffer[i].base;
    ASSERT_GT(buffer[i].size, 0u);
  }

  END_TEST;
}

static void SetPageStateRange(enum vm_page_state state, vm_page_t* start, int count) {
  for (int i = 0; i < count; ++i) {
    (start + i)->set_state(state);
  }
}

static bool pmm_arena_find_free_contiguous_test() {
  BEGIN_TEST;

  static constexpr size_t kNumPages = 8;
  const vaddr_t base = 0x1001000;
  const pmm_arena_info_t info{"test arena", 0, base, kNumPages * PAGE_SIZE};

  vm_page_t page_array[kNumPages]{};
  PmmArena arena;
  arena.InitForTest(info, page_array);

  // page_array is as follow (0 == free, 1 == allocated):
  //
  // [00000000]
  //
  // Ask for some sizes and alignments that can't possibly succeed.
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(kNumPages + 1, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(kNumPages + 2, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(kNumPages + 3, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(kNumPages + 4, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(1, 24));  // 16MB aligned
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(1, 25));  // 32MB aligned
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(1, 26));  // 64MB aligned
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(1, 27));  // 128MB aligned

  // [00000000]
  //
  // Ask for 4 pages,  aligned on a 2-page boundary.  See that the first page is skipped.
  vm_page_t* result = arena.FindFreeContiguous(4, PAGE_SIZE_SHIFT + 1);
  ASSERT_EQ(&page_array[1], result);
  SetPageStateRange(vm_page_state::ALLOC, result, 4);

  // [01111000]
  //
  // Ask for various sizes and see that they all fail.
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(4, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(5, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(6, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(7, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(8, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(9, PAGE_SIZE_SHIFT));

  // [01111000]
  //
  // Ask for 3 pages.
  result = arena.FindFreeContiguous(3, PAGE_SIZE_SHIFT);
  ASSERT_EQ(&page_array[5], result);
  SetPageStateRange(vm_page_state::ALLOC, result, 3);

  // [01111111]
  //
  // Ask for various sizes and see that they all fail.
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(2, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(3, PAGE_SIZE_SHIFT));
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(4, PAGE_SIZE_SHIFT));

  // [01111111]
  //
  // Ask for the last remaining page.
  result = arena.FindFreeContiguous(1, PAGE_SIZE_SHIFT);
  ASSERT_EQ(&page_array[0], result);
  SetPageStateRange(vm_page_state::ALLOC, result, 1);

  // [11111111]
  //
  // See there are none left.
  ASSERT_EQ(nullptr, arena.FindFreeContiguous(1, PAGE_SIZE_SHIFT));

  END_TEST;
}

// Check that when AllocPages appends to an existing list it does not run the checker on the pages
// already in the list.
static bool pmm_alloc_append_test() {
  BEGIN_TEST;

  ManagedPmmNode node;

  list_node_t alloc_list = LIST_INITIAL_VALUE(alloc_list);
  auto cleanup = fit::defer([&]() {
    if (!list_is_empty(&alloc_list)) {
      node.node().FreeList(&alloc_list);
    }
  });

  // Allocate a single page into the list first.
  ASSERT_OK(node.node().AllocPages(1, 0u, &alloc_list));

  // Zero the page as a modification.
  memset(paddr_to_physmap(list_peek_head_type(&alloc_list, vm_page_t, queue_node)->paddr()), 0,
         PAGE_SIZE);

  // Now append more pages to the list. If this runs the checker on the page already in the list
  // that we modified then it will panic.
  EXPECT_OK(node.node().AllocPages(node.kNumPages / 2, 0, &alloc_list));

  END_TEST;
}

static bool pq_add_remove() {
  BEGIN_TEST;

  PageQueues pq;

  // Pretend we have an allocated page
  vm_page_t test_page = {};
  test_page.set_state(vm_page_state::OBJECT);

  // Need a VMO to claim our pages are in
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(0, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Put the page in each queue and make sure it shows up
  pq.SetWired(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsWired(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.wired = 1}));

  pq.Remove(&test_page);
  EXPECT_FALSE(pq.DebugPageIsWired(&test_page));
  EXPECT_FALSE(pq.DebugPageIsAnonymous(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  pq.SetAnonymous(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsAnonymous(&test_page));
  if (pq.ReclaimIsOnlyPagerBacked()) {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.anonymous = 1}));
  } else {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));
  }

  pq.Remove(&test_page);
  EXPECT_FALSE(pq.DebugPageIsAnonymous(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  // Need a pager VMO to claim our page is in.
  status = make_uncommitted_pager_vmo(1, false, false, &vmo);
  ASSERT_OK(status);

  pq.SetReclaim(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));

  pq.Remove(&test_page);
  EXPECT_FALSE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  pq.SetPagerBackedDirty(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.pager_backed_dirty = 1}));

  pq.Remove(&test_page);
  EXPECT_FALSE(pq.DebugPageIsPagerBackedDirty(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  END_TEST;
}

static bool pq_move_queues() {
  BEGIN_TEST;

  PageQueues pq;

  // Pretend we have an allocated page
  vm_page_t test_page = {};
  test_page.set_state(vm_page_state::OBJECT);

  // Need a VMO to claim our pages are in
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(0, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Move the page between queues.
  pq.SetWired(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsWired(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.wired = 1}));

  pq.MoveToAnonymous(&test_page);
  EXPECT_FALSE(pq.DebugPageIsWired(&test_page));
  EXPECT_TRUE(pq.DebugPageIsAnonymous(&test_page));
  if (pq.ReclaimIsOnlyPagerBacked()) {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.anonymous = 1}));
  } else {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));
  }
  pq.Remove(&test_page);

  // Now try some pager backed queues.
  status = make_uncommitted_pager_vmo(1, false, false, &vmo);
  ASSERT_OK(status);

  pq.SetReclaim(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));

  pq.MoveToPagerBackedDirty(&test_page);
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.pager_backed_dirty = 1}));

  pq.MoveToReclaim(&test_page);
  EXPECT_TRUE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));

  pq.MoveToReclaimDontNeed(&test_page);
  EXPECT_FALSE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim_dont_need = 1}));

  // Verify that the DontNeed page is first in line for eviction.
  auto backlink = pq.PeekReclaim(PageQueues::kNumReclaim - 1);
  EXPECT_TRUE(backlink != ktl::nullopt && backlink->page == &test_page);

  pq.MoveToWired(&test_page);
  EXPECT_FALSE(pq.DebugPageIsReclaimDontNeed(&test_page));
  EXPECT_FALSE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.DebugPageIsWired(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.wired = 1}));

  pq.Remove(&test_page);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  END_TEST;
}

static bool pq_move_self_queue() {
  BEGIN_TEST;

  PageQueues pq;

  // Pretend we have an allocated page
  vm_page_t test_page = {};
  test_page.set_state(vm_page_state::OBJECT);

  // Need a VMO to claim our pages are in
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(0, 0, PAGE_SIZE, &vmo);
  ASSERT_EQ(ZX_OK, status);

  // Move the page into the queue it is already in.
  pq.SetWired(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsWired(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.wired = 1}));

  pq.MoveToWired(&test_page);
  EXPECT_TRUE(pq.DebugPageIsWired(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.wired = 1}));

  pq.Remove(&test_page);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  pq.SetAnonymous(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsAnonymous(&test_page));
  if (pq.ReclaimIsOnlyPagerBacked()) {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.anonymous = 1}));
  } else {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));
  }

  pq.MoveToAnonymous(&test_page);
  EXPECT_TRUE(pq.DebugPageIsAnonymous(&test_page));
  if (pq.ReclaimIsOnlyPagerBacked()) {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.anonymous = 1}));
  } else {
    EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));
  }

  pq.Remove(&test_page);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  // Now try some pager backed queues.
  status = make_uncommitted_pager_vmo(1, false, false, &vmo);
  ASSERT_OK(status);

  pq.SetReclaim(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));

  pq.MoveToReclaim(&test_page);
  EXPECT_TRUE(pq.DebugPageIsReclaim(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));

  pq.Remove(&test_page);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  pq.SetPagerBackedDirty(&test_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.pager_backed_dirty = 1}));

  pq.MoveToPagerBackedDirty(&test_page);
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&test_page));
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.pager_backed_dirty = 1}));

  pq.Remove(&test_page);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){}));

  END_TEST;
}

static bool pq_rotate_queue() {
  BEGIN_TEST;

  PageQueues pq;

  pq.SetActiveRatioMultiplier(0);
  pq.StartThreads(0, ZX_TIME_INFINITE);

  // Pretend we have a few allocated pages.
  vm_page_t wired_page = {};
  vm_page_t clean_pager_page = {};
  vm_page_t dirty_pager_page = {};
  wired_page.set_state(vm_page_state::OBJECT);
  clean_pager_page.set_state(vm_page_state::OBJECT);
  dirty_pager_page.set_state(vm_page_state::OBJECT);

  // Need a VMO to claim our pages are in.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = make_uncommitted_pager_vmo(1, false, false, &vmo);
  ASSERT_OK(status);

  // Put the pages in and validate initial state.
  pq.SetWired(&wired_page, vmo->DebugGetCowPages().get(), 0);
  pq.SetReclaim(&clean_pager_page, vmo->DebugGetCowPages().get(), 0);
  pq.SetPagerBackedDirty(&dirty_pager_page, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsWired(&wired_page));
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&dirty_pager_page));
  size_t queue;
  EXPECT_TRUE(pq.DebugPageIsReclaim(&clean_pager_page, &queue));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {1, 0, 0, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 1, 0}));
  EXPECT_EQ(queue, 0u);

  // Gradually rotate the queue.
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.DebugPageIsWired(&wired_page));
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&dirty_pager_page));
  EXPECT_TRUE(pq.DebugPageIsReclaim(&clean_pager_page, &queue));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 1, 0, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 1, 0}));
  EXPECT_EQ(queue, 1u);

  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 0, 1, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 1}));
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 0, 0, 1, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 1}));
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 0, 0, 0, 1, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 0, 0, 0, 0, 1, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 0, 0, 0, 0, 0, 1, 0}, .pager_backed_dirty = 1, .wired = 1}));
  pq.RotateReclaimQueues();
  // Further rotations might cause the page to be visible in the same queue, or an older one,
  // depending on whether the lru processing already ran in preparation of the next aging event.
  const PageQueues::Counts counts_last = (PageQueues::Counts){
      .reclaim = {0, 0, 0, 0, 0, 0, 0, 1}, .pager_backed_dirty = 1, .wired = 1};
  const PageQueues::Counts counts_second_last = (PageQueues::Counts){
      .reclaim = {0, 0, 1, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1};
  PageQueues::Counts counts = pq.QueueCounts();
  EXPECT_TRUE(counts == counts_last || counts == counts_second_last);

  // Further rotations should not move the page.
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.DebugPageIsWired(&wired_page));
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&dirty_pager_page));
  EXPECT_TRUE(pq.DebugPageIsReclaim(&clean_pager_page));
  counts = pq.QueueCounts();
  EXPECT_TRUE(counts == counts_last || counts == counts_second_last);
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 1}));

  // Moving the page should bring it back to the first queue.
  pq.MoveToReclaim(&clean_pager_page);
  EXPECT_TRUE(pq.DebugPageIsWired(&wired_page));
  EXPECT_TRUE(pq.DebugPageIsPagerBackedDirty(&dirty_pager_page));
  EXPECT_TRUE(pq.DebugPageIsReclaim(&clean_pager_page));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {1, 0, 0, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 1, 0}));

  // Just double check two rotations.
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 1, 0, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 1, 0}));
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){
                  .reclaim = {0, 0, 1, 0, 0, 0, 0, 0}, .pager_backed_dirty = 1, .wired = 1}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 1}));

  pq.Remove(&wired_page);
  pq.Remove(&clean_pager_page);
  pq.Remove(&dirty_pager_page);

  END_TEST;
}

static bool pq_toggle_dont_need_queue() {
  BEGIN_TEST;

  PageQueues pq;

  pq.SetActiveRatioMultiplier(0);
  pq.StartThreads(0, ZX_TIME_INFINITE);

  // Pretend we have a couple of allocated pager-backed pages.
  vm_page_t page1 = {};
  vm_page_t page2 = {};
  page1.set_state(vm_page_state::OBJECT);
  page2.set_state(vm_page_state::OBJECT);

  // Need a VMO to claim our pager backed pages are in.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = make_uncommitted_pager_vmo(2, false, false, &vmo);
  ASSERT_OK(status);

  // Put the pages in and validate initial state.
  pq.SetReclaim(&page1, vmo->DebugGetCowPages().get(), 0);
  size_t queue;
  EXPECT_TRUE(pq.DebugPageIsReclaim(&page1, &queue));
  EXPECT_EQ(queue, 0u);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {1, 0, 0, 0, 0, 0, 0, 0}}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 1, 0}));
  pq.SetReclaim(&page2, vmo->DebugGetCowPages().get(), 0);
  EXPECT_TRUE(pq.DebugPageIsReclaim(&page2, &queue));
  EXPECT_EQ(queue, 0u);
  EXPECT_TRUE(pq.QueueCounts() == ((PageQueues::Counts){.reclaim = {2, 0, 0, 0, 0, 0, 0, 0}}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 2, 0}));

  // Move the pages to the DontNeed queue.
  pq.MoveToReclaimDontNeed(&page1);
  pq.MoveToReclaimDontNeed(&page2);
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&page1));
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&page2));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){.reclaim = {0, 0, 0, 0, 0, 0, 0, 0}, .reclaim_dont_need = 2}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 2}));

  // Rotate the queues. This should also process the DontNeed queue.
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&page1));
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&page2));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){.reclaim = {0, 0, 0, 0, 0, 0, 0, 0}, .reclaim_dont_need = 2}));
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 2}));

  // Simulate access for one of the pages. Then rotate the queues again. This should move the
  // accessed page1 out of the DontNeed queue to MRU+1 (as we've rotated the queues after access).
  pq.MarkAccessed(&page1);
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.DebugPageIsReclaim(&page1, &queue));
  EXPECT_EQ(queue, 1u);
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&page2));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){.reclaim = {0, 1, 0, 0, 0, 0, 0, 0}, .reclaim_dont_need = 1}));
  // Two active queues by default, so page1 is still considered active.
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 1, 1}));

  // Rotate the queues again. The page accessed above should move to the next pager-backed queue.
  pq.RotateReclaimQueues();
  EXPECT_TRUE(pq.DebugPageIsReclaim(&page1, &queue));
  EXPECT_EQ(queue, 2u);
  EXPECT_TRUE(pq.DebugPageIsReclaimDontNeed(&page2));
  EXPECT_TRUE(pq.QueueCounts() ==
              ((PageQueues::Counts){.reclaim = {0, 0, 1, 0, 0, 0, 0, 0}, .reclaim_dont_need = 1}));
  // page1 has now moved on past the two active queues, so it now counts as inactive.
  EXPECT_TRUE(pq.GetActiveInactiveCounts() == ((PageQueues::ActiveInactiveCounts){false, 0, 2}));

  pq.Remove(&page1);
  pq.Remove(&page2);

  END_TEST;
}

static bool physmap_for_each_gap_test() {
  BEGIN_TEST;

  struct Gap {
    vaddr_t base;
    size_t size;
  };

  fbl::Vector<Gap> actual_gaps;
  fbl::AllocChecker ac;
  auto PushBack = [&](vaddr_t base, size_t size) {
    actual_gaps.push_back({base, size}, &ac);
    ASSERT(ac.check());
  };

  {
    // No arenas, [ ].
    actual_gaps.reset();
    physmap_for_each_gap(PushBack, nullptr, 0);
    // One gap covering the entire physmap.
    ASSERT_EQ(actual_gaps.size(), 1u);
    ASSERT_EQ(actual_gaps[0].base, PHYSMAP_BASE);
    ASSERT_EQ(actual_gaps[0].size, PHYSMAP_SIZE);
  }

  {
    // One arena, no gaps, [A].
    actual_gaps.reset();
    pmm_arena_info_t arenas[] = {
        {"test-arena", 0, PHYSMAP_BASE_PHYS, PHYSMAP_SIZE},
    };
    physmap_for_each_gap(PushBack, arenas, ktl::size(arenas));
    // No gaps.
    ASSERT_EQ(actual_gaps.size(), 0u);
  }

  {
    // One arena, gap at bottom, [ A].
    actual_gaps.reset();
    const size_t gap_size = 0x1000;
    const size_t arena_size = PHYSMAP_SIZE - gap_size;
    pmm_arena_info_t arenas[] = {
        {"test-arena", 0, PHYSMAP_BASE_PHYS + gap_size, arena_size},
    };
    physmap_for_each_gap(PushBack, arenas, ktl::size(arenas));
    // One gap.
    ASSERT_EQ(actual_gaps.size(), 1u);
    ASSERT_EQ(actual_gaps[0].base, PHYSMAP_BASE);
    ASSERT_EQ(actual_gaps[0].size, gap_size);
  }

  {
    // One arena, gap at top, [A ].
    actual_gaps.reset();
    const size_t gap_size = 0x5000;
    const size_t arena_size = PHYSMAP_SIZE - gap_size;
    pmm_arena_info_t arenas[] = {
        {"test-arena", 0, PHYSMAP_BASE_PHYS, arena_size},
    };
    physmap_for_each_gap(PushBack, arenas, ktl::size(arenas));
    // One gap.
    ASSERT_EQ(actual_gaps.size(), 1u);
    ASSERT_EQ(actual_gaps[0].base, PHYSMAP_BASE + arena_size);
    ASSERT_EQ(actual_gaps[0].size, gap_size);
  }

  {
    // Two arenas, no gaps, [AA].
    actual_gaps.reset();
    const size_t size = PHYSMAP_SIZE / 2;
    pmm_arena_info_t arenas[] = {
        {"test-arena", 0, PHYSMAP_BASE_PHYS, size},
        {"test-arena", 0, PHYSMAP_BASE_PHYS + size, size},
    };
    physmap_for_each_gap(PushBack, arenas, ktl::size(arenas));
    // No gaps.
    ASSERT_EQ(actual_gaps.size(), 0u);
  }

  {
    // Two arenas, three gaps, [ A A ].
    actual_gaps.reset();
    const size_t gap1_size = 0x300000;
    const size_t arena1_offset = gap1_size;
    const size_t arena1_size = 0x1000000;
    const size_t gap2_size = 0x35000;
    const size_t arena2_offset = gap1_size + arena1_size + gap2_size;
    const size_t arena2_size = 0xff1000000;
    pmm_arena_info_t arenas[] = {
        {"test-arena", 0, PHYSMAP_BASE_PHYS + arena1_offset, arena1_size},
        {"test-arena", 0, PHYSMAP_BASE_PHYS + arena2_offset, arena2_size},
    };
    physmap_for_each_gap(PushBack, arenas, ktl::size(arenas));
    // Three gaps.
    ASSERT_EQ(actual_gaps.size(), 3u);
    ASSERT_EQ(actual_gaps[0].base, PHYSMAP_BASE);
    ASSERT_EQ(actual_gaps[0].size, gap1_size);
    ASSERT_EQ(actual_gaps[1].base, PHYSMAP_BASE + arena1_offset + arena1_size);
    ASSERT_EQ(actual_gaps[1].size, gap2_size);
    const size_t arena3_offset = gap1_size + arena1_size + gap2_size + arena2_size;
    ASSERT_EQ(actual_gaps[2].base, PHYSMAP_BASE + arena3_offset);
    ASSERT_EQ(actual_gaps[2].size, PHYSMAP_SIZE - arena3_offset);
  }

  END_TEST;
}

#if __has_feature(address_sanitizer)
static bool kasan_detects_use_after_free() {
  BEGIN_TEST;
  // TODO(https://fxbug.dev/42104852): Enable on arm64 when kasan poisoning works there.
#if defined(__x86_64__)
  ManagedPmmNode node;

  zx::result<vm_page_t*> result = node.node().AllocPage(PMM_ALLOC_FLAG_ANY);
  ASSERT_EQ(ZX_OK, result.status_value(), "pmm_alloc_page one page");
  vm_page_t* page = result.value();
  paddr_t paddr = page->paddr();
  ASSERT_NE(paddr, 0UL);
  EXPECT_EQ(0UL, asan_region_is_poisoned(reinterpret_cast<uintptr_t>(paddr_to_physmap(paddr)),
                                         PAGE_SIZE));
  node.node().FreePage(page);
  EXPECT_TRUE(asan_entire_region_is_poisoned(reinterpret_cast<uintptr_t>(paddr_to_physmap(paddr)),
                                             PAGE_SIZE));
#endif
  END_TEST;
}
#endif  // __has_feature(address_sanitizer)

UNITTEST_START_TESTCASE(pmm_tests)
VM_UNITTEST(pmm_smoke_test)
VM_UNITTEST(pmm_alloc_contiguous_one_test)
VM_UNITTEST(pmm_node_multi_alloc_test)
VM_UNITTEST(pmm_node_singlton_list_test)
VM_UNITTEST(pmm_node_loan_borrow_cancel_reclaim_end)
VM_UNITTEST(pmm_node_loan_delete_lender)
VM_UNITTEST(pmm_node_oversized_alloc_test)
VM_UNITTEST(pmm_node_free_mem_event_test)
VM_UNITTEST(pmm_node_low_mem_alloc_failure_test)
VM_UNITTEST(pmm_node_explicit_should_wait_test)
VM_UNITTEST(pmm_checker_test)
VM_UNITTEST(pmm_checker_is_valid_fill_size_test)
VM_UNITTEST(pmm_get_arena_info_test)
VM_UNITTEST(pmm_arena_find_free_contiguous_test)
VM_UNITTEST(pmm_alloc_append_test)
UNITTEST_END_TESTCASE(pmm_tests, "pmm", "Physical memory manager tests")

UNITTEST_START_TESTCASE(page_queues_tests)
VM_UNITTEST(pq_add_remove)
VM_UNITTEST(pq_move_queues)
VM_UNITTEST(pq_move_self_queue)
VM_UNITTEST(pq_rotate_queue)
VM_UNITTEST(pq_toggle_dont_need_queue)
UNITTEST_END_TESTCASE(page_queues_tests, "pq", "PageQueues tests")

UNITTEST_START_TESTCASE(physmap_tests)
VM_UNITTEST(physmap_for_each_gap_test)
UNITTEST_END_TESTCASE(physmap_tests, "physmap", "physmap tests")

#if __has_feature(address_sanitizer)
UNITTEST_START_TESTCASE(kasan_pmm_tests)
VM_UNITTEST(kasan_detects_use_after_free)
UNITTEST_END_TESTCASE(kasan_pmm_tests, "kasan_pmm", "kasan pmm tests")
#endif  // __has_feature(address_sanitizer)

}  // namespace vm_unittest
