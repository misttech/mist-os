// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/stdcompat/string_view.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#include <atomic>
#include <charconv>
#include <optional>
#include <string>
#include <thread>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_number_conversions.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/proc_test_base.h"
#include "src/starnix/tests/syscalls/test_helper.h"

constexpr size_t PAGE_SIZE = 0x1000;

#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

namespace {

struct MemoryMapping {
  uintptr_t start;
  uintptr_t end;
  std::string perms;
  size_t offset;
  std::string device;
  size_t inode;
  std::string pathname;
};

std::optional<MemoryMapping> find_memory_mapping(uintptr_t addr, std::string_view maps) {
  std::vector<std::string_view> lines =
      fxl::SplitString(maps, "\n", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  // format:
  // start-end perms offset device inode path
  for (auto line : lines) {
    std::vector<std::string_view> parts =
        fxl::SplitString(line, " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    if (parts.size() < 5) {
      return std::nullopt;
    }
    std::vector<std::string_view> addrs =
        fxl::SplitString(parts[0], "-", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    if (addrs.size() != 2) {
      return std::nullopt;
    }

    uintptr_t start;
    uintptr_t end;

    if (!fxl::StringToNumberWithError(addrs[0], &start, fxl::Base::k16) ||
        !fxl::StringToNumberWithError(addrs[1], &end, fxl::Base::k16)) {
      return std::nullopt;
    }

    if (addr >= start && addr < end) {
      size_t offset;
      size_t inode;
      if (!fxl::StringToNumberWithError(parts[2], &offset, fxl::Base::k16) ||
          !fxl::StringToNumberWithError(parts[4], &inode, fxl::Base::k10)) {
        return std::nullopt;
      }

      std::string pathname;
      if (parts.size() > 5) {
        // The pathname always starts at pos 73.
        pathname = line.substr(73);
      }

      MemoryMapping mapping = {
          start, end, std::string(parts[1]), offset, std::string(parts[3]), inode, pathname,
      };

      return mapping;
    }
  }
  return std::nullopt;
}

#if __x86_64__

constexpr size_t MMAP_FILE_SIZE = 64;
constexpr intptr_t LIMIT_4GB = 0x80000000;

TEST(MmapTest, Map32Test) {
  char* tmp = getenv("TEST_TMPDIR");
  std::string path = tmp == nullptr ? "/tmp/mmaptest" : std::string(tmp) + "/mmaptest";
  int fd = open(path.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0777);
  ASSERT_GE(fd, 0);
  for (unsigned char i = 0; i < MMAP_FILE_SIZE; i++) {
    ASSERT_EQ(write(fd, &i, sizeof(i)), 1);
  }
  close(fd);

  int fdm = open(path.c_str(), O_RDWR);
  ASSERT_GE(fdm, 0);

  void* mapped =
      mmap(nullptr, MMAP_FILE_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_32BIT, fdm, 0);
  intptr_t maploc = reinterpret_cast<intptr_t>(mapped);
  intptr_t limit = LIMIT_4GB - MMAP_FILE_SIZE;
  ASSERT_GT(maploc, 0);
  ASSERT_LE(maploc, limit);

  ASSERT_EQ(munmap(mapped, MMAP_FILE_SIZE), 0);
  close(fd);

  unlink(path.c_str());
}
#endif

TEST(MmapTest, MprotectMultipleMappings) {
  char* page1 = (char*)mmap(nullptr, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(page1, MAP_FAILED) << strerror(errno);
  char* page2 = (char*)mmap(page1 + PAGE_SIZE, PAGE_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
  ASSERT_NE(page2, MAP_FAILED) << strerror(errno);
  memset(page1, 'F', PAGE_SIZE * 2);
  // This gets the starnix mapping state out of sync with the real zircon mappings...
  ASSERT_EQ(mprotect(page1, PAGE_SIZE * 2, PROT_READ), 0) << strerror(errno);
  // ...so madvise clears a page that is not mapped.
  ASSERT_EQ(madvise(page2, PAGE_SIZE, MADV_DONTNEED), 0) << strerror(errno);
  ASSERT_EQ(*page1, 'F');
  ASSERT_EQ(*page2, 0);  // would fail
}

TEST(MmapTest, MprotectSecondPageStringRead) {
  char* addr = static_cast<char*>(
      mmap(nullptr, PAGE_SIZE * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));

  mprotect(addr + PAGE_SIZE, PAGE_SIZE, 0);
  strcpy(addr, "/dev/null");
  int fd = open(addr, O_RDONLY);
  EXPECT_NE(fd, -1);
  close(fd);
  munmap(addr, PAGE_SIZE * 2);
}

TEST(MMapTest, MapFileThenGrow) {
  char* tmp = getenv("TEST_TMPDIR");
  std::string path = tmp == nullptr ? "/tmp/mmap_grow_test" : std::string(tmp) + "/mmap_grow_test";
  int fd = open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777);

  size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

  // Resize the file to be 3 pages long.
  size_t file_size = page_size * 3;
  SAFE_SYSCALL(ftruncate(fd, file_size));

  // Create a file-backed mapping that is 8 pages long.
  size_t mapping_len = page_size * 8;
  std::byte* mapping_addr = static_cast<std::byte*>(
      mmap(nullptr, mapping_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0));
  ASSERT_NE(mapping_addr, MAP_FAILED);

  // Resize the file to be 6.5 pages long.
  file_size = page_size * 6 + page_size / 2;
  SAFE_SYSCALL(ftruncate(fd, file_size));

  // Stores to the area past the original mapping should be reflected in the underlying file.
  off_t store_offset = page_size * 4;
  *reinterpret_cast<volatile std::byte*>(mapping_addr + store_offset) = std::byte{1};

  SAFE_SYSCALL(msync(mapping_addr + store_offset, page_size, MS_SYNC));
  std::byte file_value;
  SAFE_SYSCALL(pread(fd, &file_value, 1, store_offset));
  EXPECT_EQ(file_value, std::byte{1});

  // Writes to the file past the original mapping should be reflected in the mapping.
  off_t load_offset = page_size * 5;
  std::byte stored_value{2};
  SAFE_SYSCALL(pwrite(fd, &stored_value, 1, load_offset));

  SAFE_SYSCALL(msync(mapping_addr + load_offset, page_size, MS_SYNC));
  std::byte read_value = *reinterpret_cast<volatile std::byte*>(mapping_addr + load_offset);
  EXPECT_EQ(read_value, stored_value);

  // Loads and stores to the page corresponding to the end of the file work, even past the end of
  // the file.
  store_offset = file_size + 16;
  *reinterpret_cast<volatile std::byte*>(mapping_addr + store_offset) = std::byte{3};
  load_offset = store_offset;
  read_value = *reinterpret_cast<volatile std::byte*>(mapping_addr + load_offset);
  EXPECT_EQ(read_value, std::byte{3});

  // Note: https://man7.org/linux/man-pages/man2/mmap.2.html#BUGS says that stores to memory past
  // the end of the file may be visible to other memory mappings of the same file even after the
  // file is closed and unmapped.

  SAFE_SYSCALL(munmap(mapping_addr, mapping_len));

  close(fd);
  unlink(path.c_str());
}

class MMapProcTest : public ProcTestBase {};

TEST_F(MMapProcTest, CommonMappingsHavePathnames) {
  uintptr_t stack_addr = reinterpret_cast<uintptr_t>(__builtin_frame_address(0));
  uintptr_t vdso_addr = reinterpret_cast<uintptr_t>(getauxval(AT_SYSINFO_EHDR));

  std::string maps;
  ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/maps", &maps));
  auto stack_mapping = find_memory_mapping(stack_addr, maps);
  ASSERT_NE(stack_mapping, std::nullopt);
  EXPECT_EQ(stack_mapping->pathname, "[stack]");

  if (vdso_addr) {
    auto vdso_mapping = find_memory_mapping(vdso_addr, maps);
    ASSERT_NE(vdso_mapping, std::nullopt);
    EXPECT_EQ(vdso_mapping->pathname, "[vdso]");
  }
}

TEST_F(MMapProcTest, MapFileWithNewlineInName) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  char* tmp = getenv("TEST_TMPDIR");
  std::string dir = tmp == nullptr ? "/tmp" : std::string(tmp);
  std::string path = dir + "/mmap\nnewline";
  ScopedFD fd = ScopedFD(open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777));
  ASSERT_TRUE(fd);
  SAFE_SYSCALL(ftruncate(fd.get(), page_size));
  void* p = mmap(nullptr, page_size, PROT_READ, MAP_SHARED, fd.get(), 0);
  std::string address_formatted = fxl::StringPrintf("%8lx", (uintptr_t)p);

  std::string maps;
  ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/maps", &maps));
  auto mapping = find_memory_mapping(reinterpret_cast<uintptr_t>(p), maps);
  EXPECT_NE(mapping, std::nullopt);
  EXPECT_EQ(mapping->pathname, dir + "/mmap\\012newline");

  munmap(p, page_size);
  unlink(path.c_str());
}

TEST_F(MMapProcTest, MapDeletedField) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  char* tmp = getenv("TEST_TMPDIR");
  std::string dir = tmp == nullptr ? "/tmp" : std::string(tmp);
  std::string path = dir + "/tmpfile";
  ScopedFD fd = ScopedFD(open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777));
  ASSERT_TRUE(fd);
  SAFE_SYSCALL(ftruncate(fd.get(), page_size));
  void* p = mmap(nullptr, page_size, PROT_READ, MAP_SHARED, fd.get(), 0);
  std::string address_formatted = fxl::StringPrintf("%8lx", (uintptr_t)p);
  fd.reset();
  unlink(path.c_str());

  std::string maps;
  ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/maps", &maps));
  auto mapping = find_memory_mapping(reinterpret_cast<uintptr_t>(p), maps);
  EXPECT_NE(mapping, std::nullopt);
  EXPECT_EQ(mapping->pathname, dir + "/tmpfile (deleted)");

  munmap(p, page_size);
}

TEST_F(MMapProcTest, AdjacentFileMappings) {
  size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  char* tmp = getenv("TEST_TMPDIR");
  std::string dir = tmp == nullptr ? "/tmp" : std::string(tmp);
  std::string path = dir + "/mmap_test";
  int fd = open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777);
  ASSERT_GE(fd, 0);
  SAFE_SYSCALL(ftruncate(fd, page_size * 2));

  // Find two adjacent available pages in memory.
  void* p = mmap(nullptr, page_size * 2, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(MAP_FAILED, p);
  SAFE_SYSCALL(munmap(p, page_size * 2));

  // Map the first page of the file into the first page of our available space.
  ASSERT_NE(MAP_FAILED, mmap(p, page_size, PROT_READ, MAP_SHARED | MAP_FIXED, fd, 0));
  // Map the second page of the file into the second page of our available space.
  ASSERT_NE(MAP_FAILED, mmap((void*)((intptr_t)p + page_size), page_size, PROT_READ,
                             MAP_SHARED | MAP_FIXED, fd, page_size));
  std::string maps;
  ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/maps", &maps));

  // Expect one line for this file covering 2 pages

  std::vector<std::string_view> lines =
      fxl::SplitString(maps, "\n", fxl::kKeepWhitespace, fxl::kSplitWantNonEmpty);
  bool found_entry = false;
  for (auto line : lines) {
    if (cpp20::ends_with(line, path)) {
      EXPECT_FALSE(found_entry) << "extra entry found: " << line;
      found_entry = true;
    }
  }

  close(fd);
  unlink(path.c_str());
}

class MMapProcStatmTest : public ProcTestBase, public testing::WithParamInterface<int> {
 protected:
  void ReadStatm(size_t* vm_size_out, size_t* rss_size_out) {
    std::string statm;
    ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/statm", &statm));

    auto parts = SplitString(statm, " ", fxl::kTrimWhitespace, fxl::kSplitWantAll);
    EXPECT_EQ(parts.size(), 7U) << statm;

    const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
    if (vm_size_out) {
      size_t vm_size_pages = 0;
      EXPECT_TRUE(fxl::StringToNumberWithError(parts[0], &vm_size_pages)) << parts[0];
      *vm_size_out = vm_size_pages * page_size;
    }

    if (rss_size_out) {
      size_t rss_size_pages = 0;
      EXPECT_TRUE(fxl::StringToNumberWithError(parts[1], &rss_size_pages)) << parts[1];
      *rss_size_out = rss_size_pages * page_size;
    }
  }
};

TEST_P(MMapProcStatmTest, RssAfterUnmap) {
  const size_t kSize = 4 * 1024 * 1024;

  size_t vm_size_base;
  size_t rss_base;
  ASSERT_NO_FATAL_FAILURE(ReadStatm(&vm_size_base, &rss_base));

  int flags = MAP_ANON | GetParam();
  void* mapped = mmap(nullptr, kSize, PROT_READ | PROT_WRITE, flags, -1, 0);
  ASSERT_NE(mapped, nullptr) << "errno=" << errno << ", " << strerror(errno);

  size_t vm_size_mapped;
  size_t rss_mapped;
  ReadStatm(&vm_size_mapped, &rss_mapped);
  EXPECT_GT(vm_size_mapped, vm_size_base);
  EXPECT_GE(rss_mapped, rss_base);

  // Commit the allocated pages by writing some data.
  volatile char* data = reinterpret_cast<char*>(mapped);
  size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  for (size_t i = 0; i < kSize; i += page_size) {
    data[i] = 42;
  }

  size_t vm_size_committed;
  size_t rss_committed;
  ReadStatm(&vm_size_committed, &rss_committed);
  EXPECT_GT(vm_size_committed, vm_size_base);
  EXPECT_GT(rss_committed, rss_base);
  EXPECT_GT(rss_committed, rss_mapped);

  // Unmap half of the allocation.
  SAFE_SYSCALL(munmap(mapped, kSize / 2));

  size_t vm_size_unmapped_half;
  size_t rss_unmapped_half;
  ReadStatm(&vm_size_unmapped_half, &rss_unmapped_half);
  EXPECT_GT(vm_size_unmapped_half, vm_size_base);
  EXPECT_LT(vm_size_unmapped_half, vm_size_mapped);
  EXPECT_GT(rss_unmapped_half, rss_mapped);
  EXPECT_LT(rss_unmapped_half, rss_committed);

  // Unmap the rest of the allocation
  SAFE_SYSCALL(munmap(reinterpret_cast<char*>(mapped) + kSize / 2, kSize / 2));
  size_t vm_size_unmapped_all;
  size_t rss_unmapped_all;
  ReadStatm(&vm_size_unmapped_all, &rss_unmapped_all);
  EXPECT_LT(vm_size_unmapped_all, vm_size_unmapped_half);
  EXPECT_LT(rss_unmapped_all, rss_unmapped_half);
}

INSTANTIATE_TEST_SUITE_P(Private, MMapProcStatmTest, testing::Values(MAP_PRIVATE));
INSTANTIATE_TEST_SUITE_P(Shared, MMapProcStatmTest, testing::Values(MAP_SHARED));

// This variable is accessed from within a signal handler and thus must be declared volatile.
volatile void* expected_fault_address;

// The initial layout for each test is:
//
// ---- 0x00000000
//  ~~
// ---- lowest_addr_                      - start of the playground area, offset 0
//  ~~
// ---- lowest_guard_region_page          - start of guard region (not a mapping)
// 256 pages
// ---- initial_grows_down_low            - start of MAP_GROWSDOWN mapping at the start of the test
// 2 pages (initially, expected to grow)
// ---- grows_down_high                   - end of MAP_GROWSDOWN mapping
// 16 pages
// ---- highest_addr_                     - end of the playground area, offset playground_size()
class MapGrowsdownTest : public testing::Test {
 protected:
  void SetUp() override {
    page_size_ = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
    playground_size_ = 8 * 1024 * page_size_;

    // Find a large portion of unused address space to use in tests.
    void* base_addr =
        mmap(nullptr, playground_size_, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(base_addr, MAP_FAILED) << "mmap failed: " << strerror(errno) << "(" << errno << ")";
    SAFE_SYSCALL(munmap(base_addr, playground_size_));
    lowest_addr_ = static_cast<std::byte*>(base_addr);
    highest_addr_ = lowest_addr_ + playground_size_;

    // Create a new mapping with MAP_GROWSDOWN a bit below the top of the playground.
    initial_grows_down_size_ = 2 * page_size();
    initial_grows_down_low_offset_ = playground_size() - 16 * page_size();

    void* grow_initial_low_address =
        MapRelative(initial_grows_down_low_offset_, initial_grows_down_size_,
                    PROT_READ | PROT_WRITE, MAP_GROWSDOWN);
    ASSERT_NE(grow_initial_low_address, MAP_FAILED)
        << "mmap failed: " << strerror(errno) << "(" << errno << ")";
    ASSERT_EQ(grow_initial_low_address, OffsetToAddress(initial_grows_down_low_offset_));
    grows_down_high_offset_ = initial_grows_down_low_offset_ + initial_grows_down_size_;
  }

  void TearDown() override { SAFE_SYSCALL(munmap(lowest_addr_, playground_size_)); }

  void* MapRelative(size_t offset, size_t len, int prot, int flags) {
    return mmap(OffsetToAddress(offset), len, prot, flags | MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS,
                -1, 0);
  }

  // Tests that a read at |offset| within the playground generates a fault.
  bool TestThatReadSegfaults(intptr_t offset) {
    return TestThatAccessSegfaults(offset, AccessType::Read);
  }

  // Tests that a write at |offset| within the playground generates a fault.
  bool TestThatWriteSegfaults(intptr_t offset) {
    return TestThatAccessSegfaults(offset, AccessType::Write);
  }

  std::byte* OffsetToAddress(intptr_t offset) { return lowest_addr_ + offset; }

  char ReadAtOffset(intptr_t offset) {
    return std::to_integer<char>(*static_cast<volatile std::byte*>(OffsetToAddress(offset)));
  }

  void WriteAtOffset(intptr_t offset) {
    *static_cast<volatile std::byte*>(OffsetToAddress(offset)) = std::byte{};
  }

  void PrintCurrentMappingsToStderr() {
    std::string maps;
    ASSERT_TRUE(files::ReadFileToString("/proc/self/maps", &maps));
    fprintf(stderr, "Playground area is [%p, %p)\n", lowest_addr_, highest_addr_);
    fprintf(stderr, "MAP_GROWSDOWN region initially mapped to [%p, %p)\n",
            OffsetToAddress(initial_grows_down_low_offset_),
            OffsetToAddress(grows_down_high_offset_));
    fprintf(stderr, "%s\n", maps.c_str());
  }

  size_t page_size() const { return page_size_; }
  size_t playground_size() const { return playground_size_; }

  size_t initial_grows_down_size() const { return initial_grows_down_size_; }
  intptr_t initial_grows_down_low_offset() const { return initial_grows_down_low_offset_; }
  intptr_t grows_down_high_offset() const { return grows_down_high_offset_; }

  std::byte* lowest_addr() const { return lowest_addr_; }
  std::byte* highest_addr() const { return highest_addr_; }

 private:
  enum AccessType { Read, Write };

  bool TestThatAccessSegfaults(intptr_t offset, AccessType type) {
    std::byte* test_address = OffsetToAddress(offset);
    ForkHelper helper;
    helper.RunInForkedProcess([test_address, type] {
      struct sigaction segv_act;
      segv_act.sa_sigaction = [](int signo, siginfo_t* info, void* ucontext) {
        // TODO(https://fxbug.dev/118860): si_addr is not populated in Starnix. Add this check when
        // it's fixed.
        if (signo == SIGSEGV /*&& info->si_addr == expected_fault_address*/) {
          _exit(EXIT_SUCCESS);
        }
        _exit(EXIT_FAILURE);
      };
      segv_act.sa_flags = SA_SIGINFO;
      SAFE_SYSCALL(sigaction(SIGSEGV, &segv_act, nullptr));
      expected_fault_address = test_address;
      if (type == AccessType::Read) {
        *static_cast<volatile std::byte*>(test_address);
      } else {
        *static_cast<volatile std::byte*>(test_address) = std::byte{};
      }
      ADD_FAILURE() << "Expected to fault on access of " << test_address;
      exit(EXIT_FAILURE);
    });
    return helper.WaitForChildren();
  }

  size_t page_size_;
  size_t initial_grows_down_size_;
  intptr_t initial_grows_down_low_offset_;
  intptr_t grows_down_high_offset_;
  size_t playground_size_;
  std::byte* lowest_addr_;
  std::byte* highest_addr_;
};

TEST_F(MapGrowsdownTest, Grow) {
  size_t expected_guard_region_size = 256 * page_size();

  // Create a mapping 4 guard page regions below the first mapping to constrain growth.
  size_t gap_to_next_mapping = 4 * expected_guard_region_size;
  intptr_t constraint_offset = initial_grows_down_low_offset() - gap_to_next_mapping;
  void* constraint_mapping = MapRelative(constraint_offset, page_size(), PROT_NONE, 0);
  ASSERT_NE(constraint_mapping, MAP_FAILED)
      << "mmap failed: " << strerror(errno) << "(" << errno << ")";

  // Read from pages sequentially in the guard regions from just below the MAP_GROWSDOWN mapping
  // down to the edge of the second mapping.
  for (size_t i = 0; i < 4 * expected_guard_region_size / page_size(); i += 128) {
    ASSERT_EQ(ReadAtOffset(initial_grows_down_low_offset() - i * page_size()), 0);
  }
  ASSERT_EQ(
      ReadAtOffset(initial_grows_down_low_offset() - 4 * expected_guard_region_size + page_size()),
      0);

  // We should have grown our MAP_GROWSDOWN mapping to touch constraint_mapping. Test by trying to
  // make a new mapping immediately above constraint_mapping with MAP_FIXED_NOREPLACE - this should
  // fail with EXXIST.
  intptr_t test_mapping_offset = constraint_offset + page_size();
  void* desired_test_mapping_address = OffsetToAddress(test_mapping_offset);
  void* rv = mmap(desired_test_mapping_address, page_size(), PROT_READ,
                  MAP_FIXED_NOREPLACE | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  EXPECT_EQ(rv, MAP_FAILED);
  EXPECT_EQ(errno, EEXIST);

  size_t expected_growsdown_final_size = initial_grows_down_size() + 4 * expected_guard_region_size;
  intptr_t final_grows_down_offset = grows_down_high_offset() - expected_growsdown_final_size;
  std::byte* final_grows_down_address = OffsetToAddress(final_grows_down_offset);
  SAFE_SYSCALL(munmap(final_grows_down_address, expected_growsdown_final_size));
  SAFE_SYSCALL(munmap(constraint_mapping, gap_to_next_mapping));
}

TEST_F(MapGrowsdownTest, TouchPageAbove) {
  // The page immediately above the MAP_GROWSDOWN region is unmapped so issuing a read should SEGV.
  ASSERT_TRUE(TestThatReadSegfaults(grows_down_high_offset()));
}

TEST_F(MapGrowsdownTest, TouchHighestGuardRegionPage) {
  intptr_t highest_guard_region_page_offset = initial_grows_down_low_offset() - page_size();
  intptr_t lowest_guard_region_page_offset = highest_guard_region_page_offset - 512 * page_size();

  // Try making a NOREPLACE mapping just below the guard region.
  intptr_t test_offset = lowest_guard_region_page_offset - page_size();
  std::byte* test_address = OffsetToAddress(test_offset);
  void* test_mapping = mmap(test_address, page_size(), PROT_READ,
                            MAP_FIXED_NOREPLACE | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(test_mapping, MAP_FAILED) << "mmap failed: " << strerror(errno) << "(" << errno << ")";
  ASSERT_EQ(test_mapping, test_address);
  SAFE_SYSCALL(munmap(test_mapping, page_size()));

  // Read from the highest guard region page. This should trigger growth of the MAP_GROWSDOWN
  // mapping by one page.
  EXPECT_EQ(ReadAtOffset(test_offset), '\0');

  // Now mapping the page we just touched should fail.
  void* rv = mmap(test_address, page_size(), PROT_READ,
                  MAP_FIXED_NOREPLACE | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_EQ(rv, MAP_FAILED);
  ASSERT_EQ(errno, EEXIST);

  // And we shouldn't be able to make a NOREPLACE mapping in the new guard region.
  test_mapping = mmap(test_address, page_size(), PROT_READ,
                      MAP_FIXED_NOREPLACE | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_EQ(test_mapping, MAP_FAILED);
  ASSERT_EQ(errno, EEXIST);
}

TEST_F(MapGrowsdownTest, MapNoreplaceInGuardRegion) {
  // Make a MAP_GROWSDOWN mapping slightly below the top of the playground area.
  size_t initial_grows_down_size = 2 * page_size();
  intptr_t grow_low_offset = playground_size() - 16 * page_size();

  void* grow_initial_low_address =
      MapRelative(grow_low_offset, initial_grows_down_size, PROT_READ, MAP_GROWSDOWN);
  ASSERT_NE(grow_initial_low_address, MAP_FAILED)
      << "mmap failed: " << strerror(errno) << "(" << errno << ")";
  ASSERT_EQ(grow_initial_low_address, OffsetToAddress(grow_low_offset));

  // The page immediately below grow_low_address is the highest guard page. Try making a new mapping
  // in this region.
  intptr_t highest_guard_region_page_offset = grow_low_offset - page_size();
  std::byte* highest_guard_region_page_address = OffsetToAddress(highest_guard_region_page_offset);
  void* rv = mmap(highest_guard_region_page_address, page_size(), PROT_READ,
                  MAP_FIXED_NOREPLACE | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_EQ(rv, highest_guard_region_page_address);

  // Now that we've mapped something else into the guard region, touching the pages below the new
  // mapping will no longer trigger growth of our MAP_GROWSDOWN section.
  intptr_t test_offset = highest_guard_region_page_offset - page_size();
  ASSERT_TRUE(TestThatReadSegfaults(test_offset));

  // Unmap our mapping in the guard region.
  SAFE_SYSCALL(munmap(highest_guard_region_page_address, page_size()));

  // Now the region is growable again.
  ASSERT_EQ(ReadAtOffset(test_offset), '\0');

  // Since we've grown the region, we can no longer map into what used to be the top of the guard
  // region.
  rv = mmap(highest_guard_region_page_address, page_size(), PROT_READ,
            MAP_FIXED_NOREPLACE | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_EQ(rv, MAP_FAILED);
  ASSERT_EQ(errno, EEXIST);
}

TEST_F(MapGrowsdownTest, MprotectBeforeGrow) {
  // Reduce the production on the low page of the growsdown region to read-only
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset()), page_size(),
                        PROT_READ | PROT_GROWSDOWN));

  // The high page of the initial region should still be writable.
  WriteAtOffset(grows_down_high_offset() - page_size());

  // Grow the region by touching a page in the guard region.
  intptr_t test_offset = initial_grows_down_low_offset() - page_size();
  ASSERT_EQ(ReadAtOffset(test_offset), 0);

  // The new page should have only PROT_READ protections as the mprotect on the bottom of the
  // growsdown region extends to new pages.
  ASSERT_TRUE(TestThatWriteSegfaults(test_offset));
}

TEST_F(MapGrowsdownTest, MprotectAfterGrow) {
  // Grow the region down by 2 pages by accessing a page in the guard region.
  intptr_t test_offset = initial_grows_down_low_offset() - 2 * page_size();
  ASSERT_EQ(ReadAtOffset(initial_grows_down_low_offset() - 2 * page_size()), 0);

  // Set protection on low page of the initial growsdown region to PROT_NONE | PROT_GROWSDOWN.
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset()), page_size(),
                        PROT_NONE | PROT_GROWSDOWN));

  // This also changes the protection of pages below the mprotect() region, so we can no longer read
  // at |test_offset|.
  ASSERT_TRUE(TestThatReadSegfaults(test_offset));
}

TEST_F(MapGrowsdownTest, MprotectMixGrowsdownAndRegular) {
  // Grow the region down by 3 pages by accessing a page in the guard region.
  intptr_t test_offset = initial_grows_down_low_offset() - 3 * page_size();
  ASSERT_EQ(ReadAtOffset(test_offset), 0);

  // Now there are 5 pages with protection PROT_READ | PROT_WRITE below grows_down_high_offset().
  // Reduce the protections on the second-lowest page to PROT_READ without the PROT_GROWSDOWN flag.
  // This applies only to the specified range of addresses - one page, in this case.
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset() - 2 * page_size()),
                        page_size(), PROT_READ));
  // The lowest page of the mapping should still be PROT_READ | PROT_WRITE
  ASSERT_EQ(ReadAtOffset(test_offset), '\0');
  WriteAtOffset(test_offset);

  // Now set the second-highest page to PROT_READ with the MAP_GROWSDOWN flag.
  // Unlike mprotect() without the PROT_GROWSDOWN flag, this protection applies from the specified
  // range down to the next manually specified protection region.
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset()), page_size(),
                        PROT_READ | PROT_GROWSDOWN));

  // This page and the page below it are now read-only.
  ASSERT_TRUE(TestThatWriteSegfaults(initial_grows_down_low_offset()));
  ASSERT_EQ(ReadAtOffset(initial_grows_down_low_offset()), '\0');

  ASSERT_TRUE(TestThatWriteSegfaults(initial_grows_down_low_offset() - page_size()));
  ASSERT_EQ(ReadAtOffset(initial_grows_down_low_offset() - page_size()), '\0');

  // The lowest page of the mapping should still be PROT_READ | PROT_WRITE.
  WriteAtOffset(test_offset);
}

TEST_F(MapGrowsdownTest, ProtectionAfterGrowWithoutProtGrowsdownFlag) {
  // Reduce protection on the lowest page of the growsdown region to PROT_READ without the
  // PROT_GROWSDOWN flag.
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset()), page_size(), PROT_READ));

  // Grow the region down by one page with a read.
  intptr_t test_offset = initial_grows_down_low_offset() - page_size();
  ASSERT_EQ(ReadAtOffset(test_offset), '\0');

  // The new page has protections PROT_READ from the bottom of the growsdown region, even though
  // that protection was specified without the PROT_GROWSDOWN flag.
  ASSERT_TRUE(TestThatWriteSegfaults(test_offset));
}

TEST_F(MapGrowsdownTest, MprotectOnAdjacentGrowsdownMapping) {
  // Create a second MAP_GROWSDOWN mapping immediately below the initial mapping with PROT_READ |
  // PROT_WRITE.
  intptr_t second_mapping_offset = initial_grows_down_low_offset() - page_size();
  void* rv = MapRelative(second_mapping_offset, page_size(), PROT_READ | PROT_WRITE, MAP_GROWSDOWN);
  ASSERT_NE(rv, MAP_FAILED) << "mmap failed: " << strerror(errno) << "(" << errno << ")";
  ASSERT_EQ(rv, OffsetToAddress(second_mapping_offset));

  // Reduce protection on top mapping with MAP_GROWSDOWN flag.
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset()), page_size(),
                        PROT_READ | PROT_GROWSDOWN));

  // Strangely enough, this applies through to the second mapping.
  ASSERT_TRUE(TestThatWriteSegfaults(second_mapping_offset));
}

TEST_F(MapGrowsdownTest, MprotectOnAdjacentNonGrowsdownMappingBelow) {
  // Create a second mapping immediately below the initial mapping with PROT_READ | PROT_WRITE.
  intptr_t second_mapping_offset = initial_grows_down_low_offset() - page_size();
  void* rv = MapRelative(second_mapping_offset, page_size(), PROT_READ | PROT_WRITE, 0);
  ASSERT_NE(rv, MAP_FAILED) << "mmap failed: " << strerror(errno) << "(" << errno << ")";
  ASSERT_EQ(rv, OffsetToAddress(second_mapping_offset));

  // Reduce protection on top mapping with PROT_GROWSDOWN flag.
  SAFE_SYSCALL(mprotect(OffsetToAddress(initial_grows_down_low_offset()), page_size(),
                        PROT_READ | PROT_GROWSDOWN));

  // The protection change does not propagate to the adjacent non-MAP_GROWSDOWN mapping so it's
  // still PROT_READ | PROT_WRITE.
  WriteAtOffset(second_mapping_offset);
}

TEST_F(MapGrowsdownTest, SyscallReadsBelowGrowsdown) {
  // This address is not in any mapping but it is just below a MAP_GROWSDOWN mapping.
  std::byte* address_below_growsdown =
      OffsetToAddress(initial_grows_down_low_offset() - page_size());
  int fds[2];
  SAFE_SYSCALL(pipe(fds));
  // This syscall should grow the region to include the address read from and insert a '\0' into the
  // pipe.
  SAFE_SYSCALL(write(fds[1], address_below_growsdown, 1));
  char buf;
  SAFE_SYSCALL(read(fds[0], &buf, 1));
  EXPECT_EQ(buf, '\0');
}

TEST_F(MapGrowsdownTest, SyscallWritesBelowGrowsdown) {
  // This address is not in any mapping but it is just below a MAP_GROWSDOWN mapping.
  std::byte* address_below_growsdown =
      OffsetToAddress(initial_grows_down_low_offset() - page_size());
  int fds[2];
  SAFE_SYSCALL(pipe(fds));
  char buf = 'a';
  SAFE_SYSCALL(write(fds[1], &buf, 1));
  // This syscall should grow the region to include the address written to and read an 'a' from the
  // pipe.
  SAFE_SYSCALL(read(fds[0], address_below_growsdown, 1));
  EXPECT_EQ(std::to_integer<char>(*address_below_growsdown), 'a');
}

TEST(Mprotect, ProtGrowsdownOnNonGrowsdownMapping) {
  size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  void* rv = mmap(NULL, page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(rv, MAP_FAILED) << "mmap failed: " << strerror(errno) << "(" << errno << ")";
  EXPECT_EQ(mprotect(rv, page_size, PROT_READ | PROT_GROWSDOWN), -1);
  EXPECT_EQ(errno, EINVAL);
}

inline int MemFdCreate(const char* name, unsigned int flags) {
  return static_cast<int>(syscall(SYS_memfd_create, name, flags));
}

// Attempts to read a byte from the given memory address.
// Returns whether the read succeeded or not.
bool TryRead(uintptr_t addr) {
  ScopedFD mem_fd(MemFdCreate("try_read", O_WRONLY));
  if (!mem_fd) {
    return false;
  }

  return write(mem_fd.get(), reinterpret_cast<void*>(addr), 1) == 1;
}

// Attempts to write a zero byte to the given memory address.
// Returns whether the write succeeded or not.
bool TryWrite(uintptr_t addr) {
  ScopedFD zero_fd(open("/dev/zero", O_RDONLY));
  if (!zero_fd) {
    return false;
  }

  return read(zero_fd.get(), reinterpret_cast<void*>(addr), 1) == 1;
}

TEST_F(MMapProcTest, MProtectIsThreadSafe) {
  ForkHelper helper;
  helper.RunInForkedProcess([&] {
    const size_t page_size = sysconf(_SC_PAGE_SIZE);
    void* mmap1 = mmap(NULL, page_size, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(mmap1, MAP_FAILED);
    uintptr_t addr = reinterpret_cast<uintptr_t>(mmap1);
    ASSERT_TRUE(TryRead(addr));
    ASSERT_FALSE(TryWrite(addr));

    std::atomic<bool> start = false;
    std::atomic<int> count = 2;

    std::thread protect_rw([addr, &start, &count, page_size]() {
      count -= 1;
      while (!start) {
      }
      ASSERT_EQ(0, mprotect(reinterpret_cast<void*>(addr), page_size, PROT_READ | PROT_WRITE));
    });

    std::thread protect_none([addr, &start, &count, page_size]() {
      count -= 1;
      while (!start) {
      }
      ASSERT_EQ(0, mprotect(reinterpret_cast<void*>(addr), page_size, PROT_NONE));
    });

    while (count != 0) {
    }
    start = true;
    protect_none.join();
    protect_rw.join();

    std::string maps;

    ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/maps", &maps));
    auto mapping = find_memory_mapping(addr, maps);
    ASSERT_NE(mapping, std::nullopt);

    std::string perms = mapping->perms;
    ASSERT_FALSE(perms.empty());

    if (cpp20::starts_with(std::string_view(perms), "---p")) {
      // protect_none was the last one. We should not be able to read nor
      // write in this mapping.
      EXPECT_FALSE(TryRead(addr));
      EXPECT_FALSE(TryWrite(addr));
    } else if (cpp20::starts_with(std::string_view(perms), "rw-p")) {
      // protect_rw was the last one. We should be able to read and write
      // in this mapping.
      EXPECT_TRUE(TryRead(addr));
      EXPECT_TRUE(TryWrite(addr));
      volatile uint8_t* ptr = reinterpret_cast<volatile uint8_t*>(addr);
      *ptr = 5;
      EXPECT_EQ(*ptr, 5);
    } else {
      ASSERT_FALSE(true) << "invalid perms for mapping: " << perms;
    }
  });
}

TEST(Mprotect, GrowTempFilePermisisons) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  char* tmp = getenv("TEST_TMPDIR");
  std::string dir = tmp == nullptr ? "/tmp" : std::string(tmp);
  std::string path = dir + "/grow_temp_file_permissions";
  {
    uint8_t buf[] = {'a'};
    ScopedFD fd = ScopedFD(open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777));
    ASSERT_TRUE(fd);
    ASSERT_EQ(write(fd.get(), &buf[0], sizeof(buf)), 1) << errno << ": " << strerror(errno);
  }
  ASSERT_EQ(0, chmod(path.c_str(), S_IRUSR | S_IRGRP | S_IROTH));

  std::string before;
  ASSERT_TRUE(files::ReadFileToString(path.c_str(), &before));

  {
    uint8_t buf[] = {'b'};
    ScopedFD fd = ScopedFD(open(path.c_str(), O_RDONLY));
    ASSERT_EQ(-1, write(fd.get(), buf, sizeof(buf)));

    void* ptr = mmap(NULL, page_size, PROT_READ, MAP_SHARED, fd.get(), 0);
    EXPECT_NE(ptr, MAP_FAILED);

    EXPECT_NE(mprotect(ptr, page_size, PROT_READ | PROT_WRITE), 0);
    ForkHelper helper;
    helper.RunInForkedProcess([ptr] { *reinterpret_cast<volatile char*>(ptr) = 'b'; });
    EXPECT_FALSE(helper.WaitForChildren());
  }
  std::string after;
  ASSERT_TRUE(files::ReadFileToString(path.c_str(), &after));
  EXPECT_EQ(before, after);
  ASSERT_EQ(0, unlink(path.c_str()));
}

TEST_F(MMapProcTest, MprotectFailureIsConsistent) {
  // Test that even if mprotect fails, we either see the new mapping or the old
  // one, and the accesses are consistent with what is reported by the kernel.
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  char* tmp = getenv("TEST_TMPDIR");
  std::string dir = tmp == nullptr ? "/tmp" : std::string(tmp);
  std::string path = dir + "/test_mprotect_consistent_failure";
  {
    uint8_t buf[] = {1};
    ScopedFD fd = ScopedFD(open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777));
    ASSERT_TRUE(fd);
    ASSERT_EQ(write(fd.get(), &buf[0], sizeof(buf)), 1);
  }
  ScopedFD fd = ScopedFD(open(path.c_str(), O_RDONLY));
  ASSERT_TRUE(fd);

  void* ptr = mmap(0, page_size * 3, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(ptr, MAP_FAILED);
  uintptr_t ptr_addr = reinterpret_cast<uintptr_t>(ptr);

  ASSERT_NE(mmap(reinterpret_cast<void*>(ptr_addr + page_size), page_size, PROT_READ,
                 MAP_SHARED | MAP_FIXED, fd.get(), 0),
            MAP_FAILED);

  ASSERT_NE(mprotect(reinterpret_cast<void*>(ptr_addr), page_size * 3,
                     PROT_READ | PROT_WRITE | PROT_EXEC),
            0);

  std::string maps;
  ASSERT_TRUE(files::ReadFileToString(proc_path() + "/self/maps", &maps));

  auto second_page = find_memory_mapping(ptr_addr + page_size, maps);
  ASSERT_NE(second_page, std::nullopt);
  EXPECT_EQ(second_page->perms, "r--s");
  EXPECT_TRUE(TryRead(ptr_addr + page_size));
  EXPECT_FALSE(TryWrite(ptr_addr + page_size));

  auto test_consistency = [](const auto& mapping, uintptr_t addr) {
    auto new_perms = "rwxp";
    auto old_perms = "---p";
    if (mapping->perms == new_perms) {
      EXPECT_TRUE(TryRead(addr));
      EXPECT_TRUE(TryWrite(addr));

      volatile uint8_t* ptr = reinterpret_cast<volatile uint8_t*>(addr);
      *ptr = 5;
      EXPECT_EQ(*ptr, 5);
    } else if (mapping->perms == old_perms) {
      EXPECT_FALSE(TryRead(addr));
      EXPECT_FALSE(TryWrite(addr));
    } else {
      ASSERT_FALSE(true) << "invalid perms for mapping: " << mapping->perms;
    }
  };

  auto first_page = find_memory_mapping(ptr_addr, maps);
  ASSERT_NE(first_page, std::nullopt);
  test_consistency(first_page, ptr_addr);

  auto third_page = find_memory_mapping(ptr_addr + page_size * 2, maps);
  ASSERT_NE(third_page, std::nullopt);
  test_consistency(third_page, ptr_addr + page_size * 2);

  munmap(ptr, page_size * 3);
  unlink(path.c_str());
}

}  // namespace
