// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/range-map.h>

#include <tuple>
#include <utility>

#include <lockdep/guard.h>
#include <zxtest/zxtest.h>

using namespace starnix_uapi;
using namespace starnix::testing;

namespace starnix {

TEST(MemoryManager, test_brk) {
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  auto mm = current_task->mm();

  auto get_range = [&mm](const UserAddress& addr) -> util::Range<UserAddress> {
    Guard<Mutex> lock(mm->mm_state_rw_lock());
#if 0
    const fbl::RefPtr<VmAddressRegionOrMapping> region_or_mapping =
        mm->state().user_vmar().get()->FindRegion(addr);
    if (region_or_mapping && region_or_mapping->is_mapping()) {
      Guard<CriticalMutex> mlock(region_or_mapping->lock());
      auto base = region_or_mapping->base_locked();
      return util::Range<UserAddress>({base, base + region_or_mapping->size_locked()});
    }
#else
    if (auto opt = mm->state().mappings().get(addr); opt) {
      return opt->first;
    }
#endif
    EXPECT_TRUE(true, "failed to find mapping");
    return util::Range<UserAddress>({0, 0});
  };

  // Initialize the program break.
  auto base_addr = mm->set_brk(*current_task, 0);
  ASSERT_FALSE(base_addr.is_error(), "failed to set initial program break");
  ASSERT_TRUE(base_addr.value() > 0);

  // Check that the initial program break actually maps some memory.
  auto range0 = get_range(base_addr.value());
  ASSERT_EQ(range0.start, base_addr.value());
  ASSERT_EQ(range0.end, base_addr.value() + static_cast<uint64_t>(PAGE_SIZE));

  // Grow the program break by a tiny amount that does not actually result in a change.
  auto addr1 = mm->set_brk(*current_task, base_addr.value() + 1ul);
  ASSERT_FALSE(addr1.is_error(), "failed to grow brk");
  ASSERT_EQ(addr1.value(), base_addr.value() + 1ul);
  auto range1 = get_range(base_addr.value());
  ASSERT_EQ(range1.start, range0.start);
  ASSERT_EQ(range1.end, range0.end);

  // Grow the program break by a non-trival amount and observe the larger mapping.
  auto addr2 = mm->set_brk(*current_task, base_addr.value() + 24893ul);
  ASSERT_FALSE(addr2.is_error(), "failed to grow brk error %u", addr2.error_value().error_code());
  ASSERT_EQ(addr2.value(), base_addr.value() + 24893ul);
  auto range2 = get_range(base_addr.value());
  ASSERT_EQ(range2.start, base_addr.value());
  ASSERT_EQ(range2.end, addr2->round_up(PAGE_SIZE).value());

  // Shrink the program break and observe the smaller mapping.
  auto addr3 = mm->set_brk(*current_task, base_addr.value() + 14832ul);
  ASSERT_FALSE(addr3.is_error(), "failed to shrink brk");
  ASSERT_EQ(addr3.value(), base_addr.value() + 14832ul);
  auto range3 = get_range(base_addr.value());
  ASSERT_EQ(range3.start, base_addr.value());
  ASSERT_EQ(range3.end, addr3->round_up(PAGE_SIZE).value());

  // Shrink the program break close to zero and observe the smaller mapping.
  auto addr4 = mm->set_brk(*current_task, base_addr.value() + 3ul);
  ASSERT_FALSE(addr4.is_error(), "failed to drastically shrink brk");
  ASSERT_EQ(addr4.value(), base_addr.value() + 3ul);
  auto range4 = get_range(base_addr.value());
  ASSERT_EQ(range4.start, base_addr.value());
  ASSERT_EQ(range4.end, addr4->round_up(PAGE_SIZE).value());

  // Shrink the program break close to zero and observe that the mapping is not entirely
  auto addr5 = mm->set_brk(*current_task, base_addr.value());
  ASSERT_FALSE(addr5.is_error(), "failed to drastically shrink brk to zero");
  ASSERT_EQ(addr5.value(), base_addr.value());
  auto range5 = get_range(base_addr.value());
  ASSERT_EQ(range5.start, base_addr.value());
  ASSERT_EQ(range5.end, addr5.value() + static_cast<uint64_t>(PAGE_SIZE));
}

TEST(MemoryManager, test_mm_exec) {
  auto result = starnix::testing::create_kernel_and_task();
  auto [kernel, current_task] = result;

  auto mm = current_task->mm();

  auto has = [&mm](UserAddress addr) -> bool {
    Guard<Mutex> lock(mm->mm_state_rw_lock());
    return mm->state().mappings().get(addr).has_value();
  };

  auto brk_addr = mm->set_brk(*current_task, 0);
  EXPECT_TRUE(brk_addr.is_ok(), "failed to set initial program break");
  ASSERT_GT(brk_addr.value(), 0);
  ASSERT_TRUE(has(brk_addr.value()));

  auto mapped_addr = map_memory(*current_task, 0, PAGE_SIZE);
  ASSERT_GT(mapped_addr, 0);
  ASSERT_TRUE(has(mapped_addr));

  /*let node = current_task.lookup_path_from_root("/".into()).unwrap();*/
  auto exec_result = mm->exec(/*node*/);
  EXPECT_TRUE(exec_result.is_ok(), "failed to exec memory manager");

  ASSERT_FALSE(has(brk_addr.value()));
  ASSERT_FALSE(has(mapped_addr));

  auto brk_addr2 = map_memory(*current_task, brk_addr.value(), PAGE_SIZE);
  ASSERT_EQ(brk_addr.value(), brk_addr2);

  auto mapped_addr2 = map_memory(*current_task, mapped_addr, PAGE_SIZE);
  ASSERT_EQ(mapped_addr, mapped_addr2);
}

TEST(MemoryManager, test_get_contiguous_mappings_at) {
  auto result = starnix::testing::create_kernel_and_task();
  auto [kernel, current_task] = result;

  auto mm = current_task->mm();

  // Create four one-page mappings with a hole between the third one and the fourth one.
  size_t page_size = PAGE_SIZE;
  size_t addr_a = mm->base_addr_.ptr() + 10 * page_size;
  size_t addr_b = mm->base_addr_.ptr() + 11 * page_size;
  size_t addr_c = mm->base_addr_.ptr() + 12 * page_size;
  size_t addr_d = mm->base_addr_.ptr() + 14 * page_size;

  ASSERT_EQ(map_memory(*current_task, addr_a, PAGE_SIZE).ptr(), addr_a);
  ASSERT_EQ(map_memory(*current_task, addr_b, PAGE_SIZE).ptr(), addr_b);
  ASSERT_EQ(map_memory(*current_task, addr_c, PAGE_SIZE).ptr(), addr_c);
  ASSERT_EQ(map_memory(*current_task, addr_d, PAGE_SIZE).ptr(), addr_d);

  {
    Guard<Mutex> lock(mm->mm_state_rw_lock());
    // Verify that requesting an unmapped address returns an empty iterator.
    ASSERT_TRUE(mm->state().get_contiguous_mappings_at(addr_a - 100, 50)->empty());
    ASSERT_TRUE(mm->state().get_contiguous_mappings_at(addr_a - 100, 200)->empty());

    // Verify that requesting zero bytes returns an empty iterator.
    ASSERT_TRUE(mm->state().get_contiguous_mappings_at(addr_a, 0)->empty());

// Verify errors.
#if 0
    ASSERT_EQ(mm->state().get_contiguous_mappings_at(UserAddress(100), SIZE_MAX).error_value(),
              errno(EFAULT));

    ASSERT_EQ(
        mm->state().get_contiguous_mappings_at(mm->state().max_address() + 1, 0).error_value(),
        errno(EFAULT));
#endif
  }
#if STARNIX_ANON_ALLOCS
  {}
#else
  {
    ASSERT_EQ(mm->get_mapping_count(), 4);

    Guard<Mutex> lock(mm->mm_state_rw_lock());

    auto [map_a, map_b, map_c, map_d] =
        [&mm]() TA_REQ(
            mm->mm_state_rw_lock_) -> std::tuple<fbl::RefPtr<Mapping>, fbl::RefPtr<Mapping>,
                                                 fbl::RefPtr<Mapping>, fbl::RefPtr<Mapping>> {
      auto map = mm->state().mappings_.iter();
      auto it = map.begin();
      return std::make_tuple((*it).second, (*++it).second, (*++it).second, (*++it).second);
    }();

    // Verify result when requesting a whole mapping or portions of it.
    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a, page_size).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size}}));

    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a, page_size / 2).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size / 2}}));

    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a + page_size / 2, page_size / 2).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size / 2}}));

    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a + page_size / 4, page_size / 8).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size / 8}}));

    // Verify result when requesting a range spanning more than one mapping.
    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a + page_size / 2, page_size).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size / 2},
                                                                    {map_b, page_size / 2}}));

    ASSERT_EQ(
        mm->state().get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 3 / 2).value(),
        (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size / 2},
                                                              {map_b, page_size}}));

    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a, page_size * 3 / 2).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_a, page_size},
                                                                    {map_b, page_size / 2}}));

    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 2).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{
                  {map_a, page_size / 2}, {map_b, page_size}, {map_c, page_size / 2}}));

    ASSERT_EQ(
        mm->state().get_contiguous_mappings_at(addr_b + page_size / 2, page_size * 3 / 2).value(),
        (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_b, page_size / 2},
                                                              {map_c, page_size}}));

    // Verify that results stop if there is a hole.
    ASSERT_EQ(
        mm->state().get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10).value(),
        (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{
            {map_a, page_size / 2}, {map_b, page_size}, {map_c, page_size}}));

    // Verify that results stop at the last mapped page.
    ASSERT_EQ(mm->state().get_contiguous_mappings_at(addr_d, page_size * 10).value(),
              (std::vector<std::pair<fbl::RefPtr<Mapping>, size_t>>{{map_d, page_size}}));
  }
#endif
}

TEST(MemoryManager, test_unmap_returned_mappings) {
  auto result = starnix::testing::create_kernel_and_task();
  auto [kernel, current_task] = result;
  auto mm = current_task->mm();
  auto addr = map_memory(*current_task, 0, PAGE_SIZE * 2);

  std::vector<fbl::RefPtr<Mapping>> released_mappings;
  Guard<Mutex> lock(mm->mm_state_rw_lock());
  auto unmap_result = mm->state().unmap(addr, PAGE_SIZE, released_mappings);
  ASSERT_TRUE(unmap_result.is_ok());
  ASSERT_EQ(released_mappings.size(), 1);
}

TEST(MemoryManager, test_unmap_returns_multiple_mappings) {
  auto result = starnix::testing::create_kernel_and_task();
  auto [kernel, current_task] = result;
  auto mm = current_task->mm();
  auto addr = map_memory(*current_task, 0, PAGE_SIZE);
  map_memory(*current_task, addr.ptr() + 2 * PAGE_SIZE, PAGE_SIZE);

  std::vector<fbl::RefPtr<Mapping>> released_mappings;
  Guard<Mutex> lock(mm->mm_state_rw_lock());
  auto unmap_result = mm->state().unmap(addr, PAGE_SIZE * 3, released_mappings);
  ASSERT_TRUE(unmap_result.is_ok());
  ASSERT_EQ(released_mappings.size(), 2);
}

}  // namespace starnix
