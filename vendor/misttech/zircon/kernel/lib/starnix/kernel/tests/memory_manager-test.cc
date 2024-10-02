// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/cprng.h>
#include <lib/mistos/util/range-map.h>
#include <lib/unittest/unittest.h>
#include <lib/unittest/user_memory.h>

#include <algorithm>

#include <arch/defines.h>
#include <fbl/alloc_checker.h>
#include <ktl/optional.h>
#include <lockdep/guard.h>

#include <linux/prctl.h>

namespace unit_testing {

using starnix::ASPACE_HIGHEST_ADDRESS;
using starnix::FsString;
using starnix::Mapping;
using starnix::MappingBacking;
using starnix::MappingBackingMemory;
using starnix::MappingNameType;
using starnix::MemoryObject;
using starnix::PrivateAnonymous;
using starnix::testing::create_kernel_task_and_unlocked;
using starnix::testing::create_task;
using starnix::testing::map_memory;
using starnix_uapi::UserRef;

namespace {

bool test_brk() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  // Look up the given addr in the mappings table.
  auto get_range =
      [&mm](
          const UserAddress& addr) -> ktl::optional<ktl::pair<util::Range<UserAddress>, Mapping>> {
    auto state = mm->state.Read();
    if (auto opt = state->mappings.get(addr); opt) {
      return ktl::pair(opt->first, opt->second);
    }
    return ktl::nullopt;
  };

  // Initialize the program break.
  auto brk_result = mm->set_brk(*current_task, mtl::DefaultConstruct<UserAddress>());
  ASSERT_FALSE(brk_result.is_error(), "failed to set initial program break");
  auto base_addr = *brk_result;
  ASSERT_TRUE(base_addr > mtl::DefaultConstruct<UserAddress>());

  // Page containing the program break address should be reserved.
  auto result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should point to a reserved range");
  auto& [_, mapping] = result.value();
  ASSERT_EQ(MappingNameType::ReservedForHeap, mapping.name().type);

  // Growing it by a single byte results in that page becoming mapped.
  auto addr0 = mm->set_brk(*current_task, base_addr + 1ul);
  ASSERT_FALSE(addr0.is_error(), "failed to grow brk");
  ASSERT_TRUE(*addr0 > base_addr);
  result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should be mapped");
  auto [range0, _m0] = result.value();
  ASSERT_EQ(base_addr.ptr(), range0.start.ptr());
  ASSERT_EQ((base_addr + static_cast<uint64_t>(PAGE_SIZE)).ptr(), range0.end.ptr());

  // Grow the program break by another byte, which won't be enough to cause additional pages to be
  // mapped.
  auto addr1 = mm->set_brk(*current_task, base_addr + 2ul);
  ASSERT_FALSE(addr1.is_error(), "failed to grow brk");
  ASSERT_EQ((base_addr + 2u).ptr(), addr1->ptr());
  result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should be mapped");
  auto [range1, _m1] = result.value();
  ASSERT_EQ(range0.start.ptr(), range1.start.ptr());
  ASSERT_EQ(range0.end.ptr(), range1.end.ptr());

  // Grow the program break by a non-trival amount and observe the larger mapping.
  auto addr2 = mm->set_brk(*current_task, base_addr + 24893ul);
  ASSERT_FALSE(addr2.is_error(), "failed to grow brk");
  ASSERT_EQ((base_addr + 24893ul).ptr(), addr2->ptr());
  result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should be mapped");
  auto [range2, _m2] = result.value();
  ASSERT_EQ(base_addr.ptr(), range2.start.ptr());
  ASSERT_EQ(addr2->round_up(PAGE_SIZE)->ptr(), range2.end.ptr());

  // Shrink the program break and observe the smaller mapping.
  auto addr3 = mm->set_brk(*current_task, base_addr + 14832ul);
  ASSERT_FALSE(addr3.is_error(), "failed to shrink brk");
  ASSERT_EQ((base_addr + 14832ul).ptr(), addr3->ptr());
  result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should be mapped");
  auto [range3, _m3] = result.value();
  ASSERT_TRUE(range3.start == base_addr);
  ASSERT_TRUE(range3.end == addr3->round_up(PAGE_SIZE).value());

  // Shrink the program break close to zero and observe the smaller mapping.
  auto addr4 = mm->set_brk(*current_task, base_addr + 3ul);
  ASSERT_FALSE(addr4.is_error(), "failed to drastically shrink brk");
  ASSERT_TRUE(addr4.value() == base_addr + 3ul);
  result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should be mapped");
  auto [range4, _m4] = result.value();
  ASSERT_TRUE(range4.start == base_addr);
  ASSERT_TRUE(range4.end == addr4->round_up(PAGE_SIZE).value());

  // Shrink the program break to zero and observe that the mapping is gone and replaced by a
  // reserved memory.
  auto addr5 = mm->set_brk(*current_task, base_addr);
  ASSERT_FALSE(addr5.is_error(), "failed to drastically shrink brk to zero");
  ASSERT_TRUE(addr5.value() == base_addr);
  result = get_range(base_addr);
  ASSERT_TRUE(result.has_value(), "base_addr should point to a reserved range");
  auto [_range5, m] = result.value();
  ASSERT_EQ(MappingNameType::ReservedForHeap, m.name().type);

  END_TEST;
}

bool test_mm_exec() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  auto has = [&mm](const UserAddress& addr) -> bool {
    auto state = mm->state.Read();
    return state->mappings.get(addr).has_value();
  };

  auto brk_addr = mm->set_brk(*current_task, mtl::DefaultConstruct<UserAddress>());
  EXPECT_TRUE(brk_addr.is_ok(), "failed to set initial program break");
  ASSERT_TRUE(brk_addr > mtl::DefaultConstruct<UserAddress>());

  // Allocate a single page of BRK space, so that the break base address is mapped.
  auto _ = mm->set_brk(*current_task, brk_addr.value() + 1u);
  ASSERT_TRUE(has(brk_addr.value()), "failed to grow program break");

  auto mapped_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ASSERT_TRUE(mapped_addr > mtl::DefaultConstruct<UserAddress>());
  ASSERT_TRUE(has(mapped_addr));

  auto node = (*current_task).lookup_path_from_root("/").value();
  auto exec_result = mm->exec(node);
  EXPECT_TRUE(exec_result.is_ok(), "failed to exec memory manager");

  ASSERT_FALSE(has(brk_addr.value()));
  ASSERT_FALSE(has(mapped_addr));

  // Check that the old addresses are actually available for mapping.
  auto brk_addr2 = map_memory(*current_task, brk_addr.value(), PAGE_SIZE);
  ASSERT_EQ(brk_addr->ptr(), brk_addr2.ptr());
  auto mapped_addr2 = map_memory(*current_task, mapped_addr, PAGE_SIZE);
  ASSERT_EQ(mapped_addr.ptr(), mapped_addr2.ptr());

  END_TEST;
}
}  // namespace

bool test_get_contiguous_mappings_at() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  // Create four one-page mappings with a hole between the third one and the fourth one.
  size_t page_size = PAGE_SIZE;
  auto addr_a = mm->base_addr + 10 * page_size;
  auto addr_b = mm->base_addr + 11 * page_size;
  auto addr_c = mm->base_addr + 12 * page_size;
  auto addr_d = mm->base_addr + 14 * page_size;

  ASSERT_EQ(addr_a.ptr(), map_memory(*current_task, addr_a, PAGE_SIZE).ptr());
  ASSERT_EQ(addr_b.ptr(), map_memory(*current_task, addr_b, PAGE_SIZE).ptr());
  ASSERT_EQ(addr_c.ptr(), map_memory(*current_task, addr_c, PAGE_SIZE).ptr());
  ASSERT_EQ(addr_d.ptr(), map_memory(*current_task, addr_d, PAGE_SIZE).ptr());

  {
    auto mm_state = mm->state.Read();

    // Verify that requesting an unmapped address returns an empty iterator.
    ASSERT_TRUE(mm_state->get_contiguous_mappings_at(addr_a - 100ul, 50)->is_empty());
    ASSERT_TRUE(mm_state->get_contiguous_mappings_at(addr_a - 100ul, 200)->is_empty());

    // Verify that requesting zero bytes returns an empty iterator.
    ASSERT_TRUE(mm_state->get_contiguous_mappings_at(addr_a, 0)->is_empty());

    // Verify errors
    ASSERT_TRUE(errno(EFAULT) ==
                mm_state->get_contiguous_mappings_at(UserAddress(100), SIZE_MAX).error_value());

    ASSERT_TRUE(
        errno(EFAULT) ==
        mm_state->get_contiguous_mappings_at(mm_state->max_address() + 1ul, 0).error_value());
  }

#if STARNIX_ANON_ALLOCS
  {
  }
#else
  {
    ASSERT_EQ(4u, mm->get_mapping_count());

    auto mm_state = mm->state.Read();

    auto [map_a, map_b, map_c,
          map_d] = [&mm_state]() -> std::tuple<Mapping, Mapping, Mapping, Mapping> {
      auto map = mm_state->mappings.iter();
      auto it = map.begin();
      return std::make_tuple((*it).second, (*++it).second, (*++it).second, (*++it).second);
    }();

    fbl::AllocChecker ac;
    fbl::Vector<ktl::pair<Mapping, size_t>> expected;

    // Verify result when requesting a whole mapping or portions of it.
    expected.push_back({map_a, page_size}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(expected[0] == mm_state->get_contiguous_mappings_at(addr_a, page_size).value()[0]);

    expected.reset();
    expected.push_back({map_a, page_size / 2}, &ac);
    ASSERT(ac.check());
    ASSERT_TRUE(expected[0] ==
                mm_state->get_contiguous_mappings_at(addr_a, page_size / 2).value()[0]);

    ASSERT_TRUE(
        expected[0] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size / 2).value()[0]);

    expected.reset();
    expected.push_back({map_a, page_size / 8}, &ac);
    ASSERT(ac.check());
    ASSERT_TRUE(
        expected[0] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 4, page_size / 8).value()[0]);

    // Verify result when requesting a range spanning more than one mapping.
    expected.reset();
    expected.push_back({map_a, page_size / 2}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_b, page_size / 2}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(expected[0] ==
                mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size).value()[0]);
    ASSERT_TRUE(expected[1] ==
                mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size).value()[1]);

    expected.reset();
    expected.push_back({map_a, page_size / 2}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_b, page_size}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(
        expected[0] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 3 / 2).value()[0]);
    ASSERT_TRUE(
        expected[1] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 3 / 2).value()[1]);

    expected.reset();
    expected.push_back({map_a, page_size}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_b, page_size / 2}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(expected[0] ==
                mm_state->get_contiguous_mappings_at(addr_a, page_size * 3 / 2).value()[0]);
    ASSERT_TRUE(expected[1] ==
                mm_state->get_contiguous_mappings_at(addr_a, page_size * 3 / 2).value()[1]);

    expected.reset();
    expected.push_back({map_a, page_size / 2}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_b, page_size}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_c, page_size / 2}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(
        expected[0] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 2).value()[0]);
    ASSERT_TRUE(
        expected[1] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 2).value()[1]);
    ASSERT_TRUE(
        expected[2] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 2).value()[2]);

    expected.reset();
    expected.push_back({map_b, page_size / 2}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_c, page_size}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(
        expected[0] ==
        mm_state->get_contiguous_mappings_at(addr_b + page_size / 2, page_size * 3 / 2).value()[0]);
    ASSERT_TRUE(
        expected[1] ==
        mm_state->get_contiguous_mappings_at(addr_b + page_size / 2, page_size * 3 / 2).value()[1]);

    // Verify that results stop if there is a hole.
    expected.reset();
    expected.push_back({map_a, page_size / 2}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_b, page_size}, &ac);
    ASSERT(ac.check());
    expected.push_back({map_c, page_size}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(
        expected[0] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10).value()[0]);
    ASSERT_TRUE(
        expected[1] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10).value()[1]);
    ASSERT_TRUE(
        expected[2] ==
        mm_state->get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10).value()[2]);

    // Verify that results stop at the last mapped page.
    expected.reset();
    expected.push_back({map_d, page_size}, &ac);
    ASSERT(ac.check());

    ASSERT_TRUE(expected[0] ==
                mm_state->get_contiguous_mappings_at(addr_d, page_size * 10).value()[0]);
  }
#endif
  END_TEST;
}

bool test_read_write_crossing_mappings() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();
  auto ma = *current_task;

  // Map two contiguous pages at fixed addresses, but backed by distinct mappings.
  size_t page_size = PAGE_SIZE;
  auto addr = mm->base_addr + 10 * page_size;
  ASSERT_EQ(addr.ptr(), map_memory(*current_task, addr, page_size).ptr());
  ASSERT_EQ((addr + page_size).ptr(), map_memory(*current_task, addr + page_size, page_size).ptr());
#if STARNIX_ANON_ALLOCS
  ASSERT_EQ(1, mm->get_mapping_count());
#else
  ASSERT_EQ(2u, mm->get_mapping_count());
#endif

  // Write a pattern crossing our two mappings.
  auto test_addr = addr + page_size / 2;
  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> data;
  data.reserve(page_size, &ac);
  ASSERT(ac.check());

  std::ranges::generate(data, [i = 0]() mutable { return static_cast<uint8_t>(i++ % 256); });

  ASSERT_TRUE(ma.write_memory(test_addr, {data.begin(), data.end()}).is_ok(),
              "failed to write test data");

  auto read_result = ma.read_memory_to_vec(test_addr, data.size());
  ASSERT_FALSE(read_result.is_error(), "failed to read test data");
  ASSERT_BYTES_EQ(data.data(), read_result.value().data(), data.size());

  END_TEST;
}

bool test_read_write_errors() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto ma = *current_task;

  size_t page_size = PAGE_SIZE;
  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), page_size);
  fbl::Vector<uint8_t> buf;
  fbl::AllocChecker ac;
  buf.resize(page_size, &ac);
  ASSERT(ac.check());

  // Verify that accessing data that is only partially mapped is an error.
  auto partial_addr_before = addr - page_size / 2;
  ASSERT_EQ(
      errno(EFAULT).error_code(),
      ma.write_memory(partial_addr_before, {buf.data(), buf.size()}).error_value().error_code());
  ASSERT_EQ(errno(EFAULT).error_code(),
            ma.read_memory_to_vec(partial_addr_before, buf.size()).error_value().error_code());
  auto partial_addr_after = addr + page_size / 2;
  ASSERT_EQ(
      errno(EFAULT).error_code(),
      ma.write_memory(partial_addr_after, {buf.data(), buf.size()}).error_value().error_code());
  ASSERT_EQ(errno(EFAULT).error_code(),
            ma.read_memory_to_vec(partial_addr_after, buf.size()).error_value().error_code());

  // Verify that accessing unmapped memory is an error.
  auto unmapped_addr = addr + 10 * page_size;
  ASSERT_EQ(errno(EFAULT).error_code(),
            ma.write_memory(unmapped_addr, {buf.data(), buf.size()}).error_value().error_code());
  ASSERT_EQ(errno(EFAULT).error_code(),
            ma.read_memory_to_vec(unmapped_addr, buf.size()).error_value().error_code());

  // However, accessing zero bytes in unmapped memory is not an error.
  ASSERT_FALSE(ma.write_memory(unmapped_addr, {(uint8_t*)nullptr, 0}).is_error(),
               "failed to write no data");
  ASSERT_FALSE(ma.read_memory_to_vec(unmapped_addr, 0).is_error(), "failed to read no data");

  END_TEST;
}

namespace {

bool test_read_c_string_to_vec_large() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();
  auto ma = *current_task;

  size_t page_size = PAGE_SIZE;
  auto max_size = 4 * page_size;
  auto addr = mm->base_addr + 10 * page_size;

  ASSERT_EQ(addr.ptr(), map_memory(*current_task, addr, max_size).ptr());

  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> random_data;
  random_data.resize(max_size, &ac);
  ASSERT(ac.check());
  cprng_draw(random_data.data(), max_size);
  // Remove all NUL bytes.
  for (unsigned char& i : random_data) {
    if (i == 0) {
      i = 1;
    }
  }
  random_data[max_size - 1] = 0;

  auto write_result = ma.write_memory(addr, {random_data.data(), random_data.size()});
  ASSERT_TRUE(write_result.is_ok(), "failed to write test string");

  // We should read the same value minus the last byte (NUL char).
  auto read_result = ma.read_c_string_to_vec(addr, max_size);
  ASSERT_TRUE(read_result.is_ok(), "failed to read c string");

  // ASSERT_EQ(FString((char*)random_data.data(), max_size - 1), read_result.value());

  END_TEST;
}

bool test_read_c_string_to_vec() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();
  auto ma = *current_task;

  size_t page_size = PAGE_SIZE;
  auto max_size = 2 * page_size;
  auto addr = mm->base_addr + 10 * page_size;

  // Map a page at a fixed address and write an unterminated string at the end of it.
  ASSERT_TRUE(addr == map_memory(*current_task, addr, page_size));

  ktl::span<const uint8_t> test_str(reinterpret_cast<const uint8_t*>("foo!"), 4);
  auto test_addr = addr + page_size - test_str.size();
  ASSERT_TRUE(ma.write_memory(test_addr, test_str).is_ok(), "failed to write test string");

  // Expect error if the string is not terminated.
  ASSERT_EQ(errno(ENAMETOOLONG).error_code(),
            ma.read_c_string_to_vec(test_addr, max_size).error_value().error_code());

  // Expect success if the string is terminated.
  ASSERT_TRUE(ma.write_memory(addr + (page_size - 1), {(uint8_t*)"\0", 1}).is_ok(),
              "failed to write test string");

  auto string = ma.read_c_string_to_vec(test_addr, max_size);
  ASSERT_TRUE(string.is_ok());
  ASSERT_TRUE(FsString("foo") == string.value());

  // Expect success if the string spans over two mappings.
  ASSERT_TRUE((addr + page_size) == map_memory(*current_task, addr + page_size, page_size));
  // TODO (Herrera): Adjacent private anonymous mappings are collapsed. To test this case this test
  // needs to provide a backing for the second mapping. assert_eq!(mm.get_mapping_count(), 2);
  ASSERT_TRUE(ma.write_memory(addr + (page_size - 1), {(const uint8_t*)"bar\0", 4}).is_ok(),
              "failed to write extra chars");

  auto string2 = ma.read_c_string_to_vec(test_addr, max_size);
  ASSERT_TRUE(string2.is_ok());
  // ASSERT_BYTES_EQ(FsString("foobar").data(), string2->data(), string2->size());

  // Expect error if the string exceeds max limit
  ASSERT_EQ(errno(ENAMETOOLONG).error_code(),
            ma.read_c_string_to_vec(test_addr, 2).error_value().error_code());

  // Expect error if the address is invalid.
  ASSERT_EQ(errno(EFAULT).error_code(),
            ma.read_c_string_to_vec(mtl::DefaultConstruct<UserCString>(), max_size)
                .error_value()
                .error_code());

  END_TEST;
}

bool test_read_c_string() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();
  auto ma = *current_task;

  size_t page_size = PAGE_SIZE;
  auto buf_cap = 2u * page_size;
  auto addr = mm->base_addr + 10u * page_size;

  auto vec = fbl::Vector<uint8_t>();
  fbl::AllocChecker ac;
  vec.reserve(buf_cap, &ac);
  ASSERT(ac.check());

  // Map a page at a fixed address and write an unterminated string at the end of it.
  ASSERT_TRUE(addr == map_memory(*current_task, addr, page_size));
  ktl::string_view test_str("foo!");
  auto test_addr = addr + page_size - test_str.size();
  ASSERT_FALSE(ma.write_memory(test_addr, {(uint8_t*)test_str.data(), test_str.size()}).is_error(),
               "failed to write test string");

  // Expect error if the string is not terminated.
  ktl::span span{vec.data(), buf_cap};
  ASSERT_TRUE(errno(ENAMETOOLONG) == ma.read_c_string(UserCString(test_addr), span).error_value());

  // Expect success if the string is terminated.
  ASSERT_FALSE(ma.write_memory(addr + (page_size - 1), {(uint8_t*)"\0", 1}).is_error(),
               "failed to write nul");
  ASSERT_TRUE(FsString("foo") == ma.read_c_string(UserCString(test_addr), span).value());

  // Expect success if the string spans over two mappings.
  ASSERT_TRUE(addr + page_size == map_memory(*current_task, addr + page_size, page_size));
  // TODO(Herrera): To be multiple mappings we need to provide a file backing for the next page or
  // the mappings will be collapsed. assert_eq!(mm.get_mapping_count(), 2);

  ASSERT_FALSE(ma.write_memory(addr + (page_size - 1), {(uint8_t*)"bar\0", 4}).is_error(),
               "failed to write extra chars");
  ASSERT_TRUE("foobar" == ma.read_c_string(UserCString(test_addr), span).value());

  // Expect error if the string does not fit in the provided buffer.
  ktl::span small_span{vec.data(), 2};
  ASSERT_TRUE(errno(ENAMETOOLONG) ==
              ma.read_c_string(UserCString(test_addr), small_span).error_value());

  // Expect error if the address is invalid.
  ASSERT_TRUE(errno(EFAULT) ==
              ma.read_c_string(mtl::DefaultConstruct<UserCString>(), span).error_value());

  END_TEST;
}

bool test_find_next_unused_range() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  auto mmap_top = mm->state.Read()->find_next_unused_range(0).value().ptr();
  auto page_size = static_cast<size_t>(PAGE_SIZE);
  ASSERT(mmap_top <= ASPACE_HIGHEST_ADDRESS);

  // No mappings - top address minus requested size is available
  ASSERT_EQ(mm->state.Read()->find_next_unused_range(page_size).value().ptr(),
            UserAddress::from_ptr(mmap_top - page_size).ptr());

  // Fill it.
  auto addr = UserAddress::from_ptr(mmap_top - page_size);
  ASSERT_EQ(map_memory(*current_task, addr, PAGE_SIZE).ptr(), addr.ptr());

  // The next available range is right before the new mapping.
  ASSERT_EQ(mm->state.Read()->find_next_unused_range(page_size).value().ptr(),
            UserAddress::from_ptr(addr.ptr() - page_size).ptr());

  // Allocate an extra page before a one-page gap.
  auto addr2 = UserAddress::from_ptr(addr.ptr() - (2 * page_size));
  ASSERT_EQ(map_memory(*current_task, addr2, PAGE_SIZE).ptr(), addr2.ptr());

  // Searching for one-page range still gives the same result
  ASSERT_EQ(mm->state.Read()->find_next_unused_range(page_size).value().ptr(),
            UserAddress::from_ptr(addr.ptr() - page_size).ptr());

  // Searching for a bigger range results in the area before the second mapping
  ASSERT_EQ(mm->state.Read()->find_next_unused_range(2 * page_size).value().ptr(),
            UserAddress::from_ptr(addr2.ptr() - (2 * page_size)).ptr());

  // Searching for more memory than available should fail.
  ASSERT_TRUE(mm->state.Read()->find_next_unused_range(mmap_top) == ktl::nullopt);

  END_TEST;
}

}  // namespace

bool test_unmap_returned_mappings() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(),
                         static_cast<uint64_t>(PAGE_SIZE) * 2);

  fbl::Vector<Mapping> released_mappings;
  auto unmap_result =
      mm->state.Write()->unmap(mm, addr, static_cast<size_t>(PAGE_SIZE), released_mappings);
  ASSERT_TRUE(unmap_result.is_ok());
  ASSERT_EQ(1u, released_mappings.size());

  END_TEST;
}

bool test_unmap_returns_multiple_mappings() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  // find_next_unused_range
  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  map_memory(*current_task, addr + (2 * static_cast<uint64_t>(PAGE_SIZE)), PAGE_SIZE);

  fbl::Vector<Mapping> released_mappings;
  auto unmap_result =
      mm->state.Write()->unmap(mm, addr, static_cast<size_t>(PAGE_SIZE) * 3, released_mappings);
  ASSERT_TRUE(unmap_result.is_ok());
  ASSERT_EQ(2u, released_mappings.size());

  END_TEST;
}

/// Maps two pages, then unmaps the first page.
/// The second page should be re-mapped with a new child COW VMO.
bool test_unmap_beginning() {
  BEGIN_TEST;

  auto [_kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE * 2ul);

  fbl::RefPtr<MemoryObject> original_memory;
  {
    auto state = mm->state.Read();
    auto pair = state->mappings.get(addr);
    ASSERT_TRUE(pair.has_value(), "mapping");

    auto& [range, mapping] = pair.value();
    ASSERT_EQ(range.start.ptr(), addr.ptr());
    ASSERT_EQ(range.end.ptr(), (addr + (PAGE_SIZE * 2u)).ptr());

#if STARNIX_ANON_ALLOCS
// #[cfg(feature = "alternate_anon_allocs")]
//     let _ = mapping;
#else
    ktl::visit(MappingBacking::overloaded{
                   [](PrivateAnonymous&) {
                     BEGIN_TEST;
                     END_TEST;
                   },
                   [&](MappingBackingMemory& backing) {
                     BEGIN_TEST;
                     EXPECT_EQ(addr.ptr(), backing.base_.ptr());
                     EXPECT_EQ(0u, backing.memory_offset_);
                     EXPECT_EQ(PAGE_SIZE * 2ul, backing.memory_->get_size());
                     original_memory = backing.memory_;
                     END_TEST;
                   },
               },
               mapping.backing_.variant);
#endif
  }  // namespace starnix

  ASSERT_TRUE(mm->unmap(addr, PAGE_SIZE).is_ok());

  {
    auto state = mm->state.Read();

    // The first page should be unmapped.
    ASSERT_FALSE(state->mappings.get(addr).has_value());

    // The second page should be a new child COW VMO.
    auto pair = state->mappings.get(addr + static_cast<size_t>(PAGE_SIZE));
    ASSERT_TRUE(pair.has_value(), "second page");
    auto& [range, mapping] = pair.value();
    ASSERT_EQ(range.start.ptr(), (addr + static_cast<size_t>(PAGE_SIZE)).ptr());
    ASSERT_EQ(range.end.ptr(), (addr + (PAGE_SIZE * 2u)).ptr());

#if STARNIX_ANON_ALLOCS
#else
    ktl::visit(MappingBacking::overloaded{
                   [](PrivateAnonymous&) {
                     BEGIN_TEST;
                     END_TEST;
                   },
                   [&](MappingBackingMemory& backing) {
                     BEGIN_TEST;
                     EXPECT_EQ((addr + static_cast<size_t>(PAGE_SIZE)).ptr(), backing.base_.ptr());
                     EXPECT_EQ(0u, backing.memory_offset_);
                     EXPECT_EQ(static_cast<size_t>(PAGE_SIZE), backing.memory_->get_size());
                     EXPECT_NE(original_memory->as_vmo()->get().dispatcher()->get_koid(),
                               backing.memory_->as_vmo()->get().dispatcher()->get_koid());
                     END_TEST;
                   },
               },
               mapping.backing_.variant);
#endif
  }

  END_TEST;
}

/// Maps two pages, then unmaps the second page.
/// The first page's VMO should be shrunk.
bool test_unmap_end() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE * 2ul);

  fbl::RefPtr<MemoryObject> original_memory;
  {
    auto state = mm->state.Read();
    auto pair = state->mappings.get(addr);
    ASSERT_TRUE(pair.has_value(), "mapping");

    auto& [range, mapping] = pair.value();
    ASSERT_EQ(range.start.ptr(), addr.ptr());
    ASSERT_EQ(range.end.ptr(), (addr + (PAGE_SIZE * 2u)).ptr());

#if STARNIX_ANON_ALLOCS
// #[cfg(feature = "alternate_anon_allocs")]
//     let _ = mapping;
#else
    ktl::visit(MappingBacking::overloaded{
                   [](PrivateAnonymous&) {
                     BEGIN_TEST;
                     END_TEST;
                   },
                   [&](MappingBackingMemory& backing) {
                     BEGIN_TEST;
                     EXPECT_EQ(addr.ptr(), backing.base_.ptr());
                     EXPECT_EQ(0u, backing.memory_offset_);
                     EXPECT_EQ(PAGE_SIZE * 2ul, backing.memory_->get_size());
                     original_memory = backing.memory_;
                     END_TEST;
                   },
               },
               mapping.backing_.variant);
#endif
  }  // namespace starnix

  ASSERT_TRUE(mm->unmap(addr + static_cast<size_t>(PAGE_SIZE), PAGE_SIZE).is_ok());

  {
    auto state = mm->state.Read();

    // The second page should be unmapped.
    ASSERT_FALSE(state->mappings.get(addr + static_cast<size_t>(PAGE_SIZE)).has_value());

    // The first page's VMO should be the same as the original, only shrunk.
    auto pair = state->mappings.get(addr);
    ASSERT_TRUE(pair.has_value(), "first page");
    auto& [range, mapping] = pair.value();
    ASSERT_EQ(range.start.ptr(), addr.ptr());
    ASSERT_EQ(range.end.ptr(), (addr + static_cast<size_t>(PAGE_SIZE)).ptr());

#if STARNIX_ANON_ALLOCS
// #[cfg(feature = "alternate_anon_allocs")]
//     let _ = mapping;
#else
    ktl::visit(MappingBacking::overloaded{
                   [](PrivateAnonymous&) {
                     BEGIN_TEST;
                     END_TEST;
                   },
                   [&](MappingBackingMemory& backing) {
                     BEGIN_TEST;
                     EXPECT_EQ(addr.ptr(), backing.base_.ptr());
                     EXPECT_EQ(0u, backing.memory_offset_);
                     EXPECT_EQ(static_cast<size_t>(PAGE_SIZE), backing.memory_->get_size());
                     END_TEST;
                   },
               },
               mapping.backing_.variant);
#endif
  }

  END_TEST;
}

/// Maps three pages, then unmaps the middle page.
/// The last page should be re-mapped with a new COW child VMO.
/// The first page's VMO should be shrunk,
bool test_unmap_middle() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();

  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE * 3ul);

  fbl::RefPtr<MemoryObject> original_memory;
  {
    auto state = mm->state.Read();
    auto pair = state->mappings.get(addr);
    ASSERT_TRUE(pair.has_value(), "mapping");
    auto& [range, mapping] = pair.value();
    ASSERT_EQ(range.start.ptr(), addr.ptr());
    ASSERT_EQ(range.end.ptr(), (addr + (PAGE_SIZE * 3u)).ptr());

#if STARNIX_ANON_ALLOCS
    // #[cfg(feature = "alternate_anon_allocs")]
    //     let _ = mapping;
#else
    ktl::visit(MappingBacking::overloaded{
                   [](PrivateAnonymous&) {
                     BEGIN_TEST;
                     END_TEST;
                   },
                   [&](MappingBackingMemory& backing) {
                     BEGIN_TEST;
                     EXPECT_EQ(addr.ptr(), backing.base_.ptr());
                     EXPECT_EQ(0u, backing.memory_offset_);
                     EXPECT_EQ(PAGE_SIZE * 3ul, backing.memory_->get_size());

                     original_memory = backing.memory_;
                     END_TEST;
                   },
               },
               mapping.backing_.variant);
#endif
  }  // namespace starnix

  ASSERT_TRUE(mm->unmap(addr + static_cast<size_t>(PAGE_SIZE), PAGE_SIZE).is_ok());

  {
    auto state = mm->state.Read();

    // The middle page should be unmapped.
    ASSERT_FALSE(state->mappings.get(addr + static_cast<size_t>(PAGE_SIZE)).has_value());

    {
      auto pair = state->mappings.get(addr);
      ASSERT_TRUE(pair.has_value(), "first page");
      auto& [range, mapping] = pair.value();
      ASSERT_EQ(range.start.ptr(), addr.ptr());
      ASSERT_EQ(range.end.ptr(), (addr + static_cast<size_t>(PAGE_SIZE)).ptr());

#if STARNIX_ANON_ALLOCS
#[cfg(feature = "alternate_anon_allocs")]
      let _ = mapping;
#else
      // The first page's memory object should be the same as the original, only shrunk.
      ktl::visit(MappingBacking::overloaded{
                     [](PrivateAnonymous&) {
                       BEGIN_TEST;
                       END_TEST;
                     },
                     [&](MappingBackingMemory& backing) {
                       BEGIN_TEST;
                       EXPECT_EQ(addr.ptr(), backing.base_.ptr());
                       EXPECT_EQ(0u, backing.memory_offset_);
                       EXPECT_EQ(static_cast<size_t>(PAGE_SIZE), backing.memory_->get_size());
                       EXPECT_EQ(original_memory->as_vmo()->get().dispatcher()->get_koid(),
                                 backing.memory_->as_vmo()->get().dispatcher()->get_koid());
                       END_TEST;
                     },
                 },
                 mapping.backing_.variant);
#endif
    }

    {
      auto pair = state->mappings.get(addr + PAGE_SIZE * 2u);
      ASSERT_TRUE(pair.has_value(), "last page");
      auto& [range, mapping] = pair.value();
      ASSERT_EQ(range.start.ptr(), (addr + (PAGE_SIZE * 2u)).ptr());
      ASSERT_EQ(range.end.ptr(), (addr + (PAGE_SIZE * 3u)).ptr());

#if STARNIX_ANON_ALLOCS
#[cfg(feature = "alternate_anon_allocs")]
      let _ = mapping;
#else
      // The last page should be a new child COW memory object.
      ktl::visit(MappingBacking::overloaded{
                     [](PrivateAnonymous&) {
                       BEGIN_TEST;
                       END_TEST;
                     },
                     [&](MappingBackingMemory& backing) {
                       BEGIN_TEST;
                       EXPECT_EQ((addr + PAGE_SIZE * 2u).ptr(), backing.base_.ptr());
                       EXPECT_EQ(0u, backing.memory_offset_);
                       EXPECT_EQ(static_cast<size_t>(PAGE_SIZE), backing.memory_->get_size());
                       EXPECT_NE(original_memory->as_vmo()->get().dispatcher()->get_koid(),
                                 backing.memory_->as_vmo()->get().dispatcher()->get_koid());
                       END_TEST;
                     },
                 },
                 mapping.backing_.variant);
#endif
    }
  }

  END_TEST;
}

namespace {

bool test_read_write_objects() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();
  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  auto ma = *current_task;
  auto item_ref = UserRef<uint32_t>::New(addr);

  auto items_written = fbl::Vector<uint32_t>();
  fbl::AllocChecker ac;
  items_written.push_back(0, &ac);
  ASSERT(ac.check());
  items_written.push_back(2, &ac);
  ASSERT(ac.check());
  items_written.push_back(3, &ac);
  ASSERT(ac.check());
  items_written.push_back(7, &ac);
  ASSERT(ac.check());
  items_written.push_back(1, &ac);
  ASSERT(ac.check());

  ASSERT_FALSE(ma.write_objects(item_ref, items_written.data(), items_written.size()).is_error(),
               "Failed to write object array.");

  auto items_read = ma.read_objects_to_vec(item_ref, items_written.size());
  ASSERT_FALSE(items_read.is_error(), "Failed to read empty object array.");

  ASSERT_EQ(items_written.size(), items_read->size());
  ASSERT_EQ(items_written[0], items_read.value()[0]);
  ASSERT_EQ(items_written[1], items_read.value()[1]);
  ASSERT_EQ(items_written[2], items_read.value()[2]);
  ASSERT_EQ(items_written[3], items_read.value()[3]);
  ASSERT_EQ(items_written[4], items_read.value()[4]);

  END_TEST;
}

bool test_read_write_objects_null() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto mm = current_task->mm();
  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  auto ma = *current_task;
  auto item_ref = UserRef<uint32_t>::New(addr);

  auto items_written = fbl::Vector<uint32_t>();

  ASSERT_FALSE(ma.write_objects(item_ref, items_written.data(), items_written.size()).is_error(),
               "Failed to write empty object array.");

  auto items_read = ma.read_objects_to_vec(item_ref, items_written.size());
  ASSERT_FALSE(items_read.is_error(), "Failed to read empty object array.");

  ASSERT_EQ(items_written.size(), items_read->size());

  END_TEST;
}

bool test_read_object_partial() {
  BEGIN_TEST;

  struct Items {
    ktl::array<uint32_t, 4> val;
  };

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto ma = *current_task;
  auto mm = current_task->mm();
  auto addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  auto item_ref = UserRef<uint32_t>::New(addr);

  // Populate some values.
  auto items_written = fbl::Vector<uint32_t>();
  fbl::AllocChecker ac;
  items_written.push_back(75, &ac);
  ASSERT(ac.check());
  items_written.push_back(23, &ac);
  ASSERT(ac.check());
  items_written.push_back(51, &ac);
  ASSERT(ac.check());
  items_written.push_back(98, &ac);
  ASSERT(ac.check());

  ASSERT_FALSE(ma.write_objects(item_ref, items_written.data(), items_written.size()).is_error(),
               "Failed to write object array.");

  // Full read of all 4 values.
  auto items_ref = UserRef<Items>::New(addr);
  auto items_read = ma.read_object_partial(items_ref, sizeof(Items));
  ASSERT_FALSE(items_read.is_error(), "Failed to read object");
  ASSERT_EQ(items_written[0], items_read->val[0]);
  ASSERT_EQ(items_written[1], items_read->val[1]);
  ASSERT_EQ(items_written[2], items_read->val[2]);
  ASSERT_EQ(items_written[3], items_read->val[3]);

  // Partial read of the first two.
  items_read = ma.read_object_partial(items_ref, 8);
  ASSERT_FALSE(items_read.is_error(), "Failed to read object");
  ASSERT_EQ(75u, items_read->val[0]);
  ASSERT_EQ(23u, items_read->val[1]);
  ASSERT_EQ(0u, items_read->val[2]);
  ASSERT_EQ(0u, items_read->val[3]);

  // The API currently allows reading 0 bytes (this could be re-evaluated) so test that does
  // the right thing.
  // Partial read of the first two.
  items_read = ma.read_object_partial(items_ref, 0);
  ASSERT_FALSE(items_read.is_error(), "Failed to read object");
  ASSERT_EQ(0u, items_read->val[0]);
  ASSERT_EQ(0u, items_read->val[1]);
  ASSERT_EQ(0u, items_read->val[2]);
  ASSERT_EQ(0u, items_read->val[3]);

  // Size bigger than the object.
  ASSERT_EQ(errno(EINVAL).error_code(),
            ma.read_object_partial(items_ref, sizeof(Items) + 8).error_value().error_code());

  // Bad pointer.
  ASSERT_EQ(errno(EFAULT).error_code(),
            ma.read_object_partial(UserRef<Items>::New(UserAddress::from(1)), 16)
                .error_value()
                .error_code());

  END_TEST;
}

}  // namespace

bool test_preserve_name_snapshot() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto name_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)"foo\0", 4}).is_ok());

  auto mapping_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);

  ASSERT_TRUE(starnix_syscalls::SUCCESS == sys_prctl(*current_task, PR_SET_VMA,
                                                     PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                                                     PAGE_SIZE, name_addr.ptr()));

  auto target = create_task(kernel, "another-task");

  auto result = current_task->mm()->snapshot_to(target->mm());
  ASSERT_TRUE(result.is_ok(), "snapshot_to failed");

  {
    auto state = target->mm()->state.Read();

    auto pair = state->mappings.get(mapping_addr);
    ASSERT_TRUE(pair.has_value());
    auto [range, mapping] = pair.value();
    // ASSERT_BYTES_EQ(fbl::String("foo"), mapping.name_.vmaName, );
  }

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_mm)
UNITTEST("test brk", unit_testing::test_brk)
UNITTEST("test mm exec", unit_testing::test_mm_exec)
UNITTEST("test get contiguous mappings at", unit_testing::test_get_contiguous_mappings_at)
UNITTEST("test read write crossing mappings", unit_testing::test_read_write_crossing_mappings)
UNITTEST("test read write errors", unit_testing::test_read_write_errors)
UNITTEST("test read c string to vec large", unit_testing::test_read_c_string_to_vec_large)
UNITTEST("test read c string to vec", unit_testing::test_read_c_string_to_vec)
UNITTEST("test read c string", unit_testing::test_read_c_string)
UNITTEST("test find next unused range", unit_testing::test_find_next_unused_range)
UNITTEST("test unmap returned mappings", unit_testing::test_unmap_returned_mappings)
UNITTEST("test unmap returns multiple mappings", unit_testing::test_unmap_returns_multiple_mappings)
UNITTEST("test read write objects", unit_testing::test_read_write_objects)
UNITTEST("test read write objects null", unit_testing::test_read_write_objects_null)
UNITTEST("test read object partial", unit_testing::test_read_object_partial)
UNITTEST("test preserve name snapshot", unit_testing::test_preserve_name_snapshot)
UNITTEST_END_TESTCASE(starnix_mm, "starnix_mm", "Tests for Memory Manager")
