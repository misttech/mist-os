// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <numeric>

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

using FileCacheTest = SingleFileTest;

TEST_F(FileCacheTest, WaitOnWriteback) {
  char buf[kPageSize];
  FileTester::AppendToFile(&vnode<File>(), buf, kPageSize);

  WritebackOperation op = {.bSync = false};
  vnode<File>().Writeback(op);
  LockedPage page = GetPage(0);
  ASSERT_TRUE(page->IsWriteback());
  page.WaitOnWriteback();
  ASSERT_FALSE(page->IsWriteback());
}

TEST_F(FileCacheTest, Map) {
  constexpr size_t kNum = 0xAA00AA00AA00AA00;
  size_t *base = nullptr;
  {
    // Get discardable backed page
    LockedPage page;
    ASSERT_EQ(fs_->GetMetaPage(0, &page), ZX_OK);
    // Set dirty to keep |page| and its mapping in FileCache.
    page.SetDirty();
    base = page->GetAddress<size_t>();
    *base = kNum;
  }
  // Even after LockedPage is destructed, the mapping should be maintained.
  ASSERT_EQ(*base, kNum);
}

TEST_F(FileCacheTest, EvictActivePages) {
  char buf[kPageSize];
  auto &file = vnode<File>();

  // Make two dirty Pages.
  FileTester::AppendToFile(&file, buf, kPageSize);
  FileTester::AppendToFile(&file, buf, kPageSize);

  // Configure op to flush dirty Pages in a sync manner.
  WritebackOperation op = {.bSync = true, .bReleasePages = false};

  constexpr uint64_t kPageNum = 2;
  fbl::RefPtr<Page> unlock_page;
  Page *raw_pages[kPageNum];
  for (auto i = 0ULL; i < kPageNum; ++i) {
    LockedPage page = GetPage(i);
    ASSERT_TRUE(page->IsDirty());
    ASSERT_FALSE(page->IsWriteback());
    raw_pages[i] = page.get();
    if (!i) {
      unlock_page = page.release();
    }
  }

  // Flush every dirty Page regardless of its active flag.
  ASSERT_EQ(file.Writeback(op), kPageNum);

  for (auto i = 0ULL; i < kPageNum; ++i) {
    LockedPage page = GetPage(i);
    ASSERT_FALSE(page->IsDirty());
  }

  // Every Page becomes inactive.
  unlock_page.reset();

  fbl::RefPtr<Page> inactive_pages[kPageNum];
  for (auto i = 0ULL; i < kPageNum; ++i) {
    ASSERT_FALSE(raw_pages[i]->IsActive());
    ASSERT_TRUE(raw_pages[i]->InTreeContainer());
    // Get the ref of Pages to avoid deletion when they are evicted from FileCache.
    inactive_pages[i] = fbl::RefPtr<Page>(raw_pages[i]);
  }

  // Evict every inactive Page.
  op.bReleasePages = true;
  op.bSync = false;
  ASSERT_EQ(file.Writeback(op), 0ULL);

  for (auto i = 0ULL; i < kPageNum; ++i) {
    ASSERT_FALSE(raw_pages[i]->IsActive());
    // Every Pages were evicted.
    ASSERT_FALSE(raw_pages[i]->InTreeContainer());
  }
}

TEST_F(FileCacheTest, WritebackOperation) {
  auto &file = vnode<File>();
  char buf[kPageSize];
  pgoff_t key;
  WritebackOperation op = {.start = 0,
                           .end = 2,
                           .to_write = 2,
                           .bSync = true,
                           .bReleasePages = false,
                           .if_page = [&key](const fbl::RefPtr<Page> &page) {
                             if (page->GetKey() <= key) {
                               return ZX_OK;
                             }
                             return ZX_ERR_NEXT;
                           }};

  // |vn| should not have any dirty Pages.
  ASSERT_EQ(file.GetDirtyPageCount(), 0U);
  FileTester::AppendToFile(&file, buf, kPageSize);
  FileTester::AppendToFile(&file, buf, kPageSize);
  // Flush the Page of 1st block.
  {
    LockedPage page = GetPage(0);
    ASSERT_EQ(file.GetDirtyPageCount(), 2U);
    key = page->GetKey();
    auto unlocked_page = page.release();
    // Request writeback for dirty Pages. |unlocked_page| should be written out.
    key = 0;
    ASSERT_EQ(file.Writeback(op), 1UL);
    // Writeback() should be able to flush |unlocked_page|.
    ASSERT_EQ(file.GetDirtyPageCount(), 1U);
    ASSERT_EQ(fs_->GetSuperblockInfo().GetPageCount(CountType::kWriteback), 0);
    ASSERT_EQ(fs_->GetSuperblockInfo().GetPageCount(CountType::kDirtyData), 1);
    ASSERT_EQ(unlocked_page->IsWriteback(), false);
    ASSERT_EQ(unlocked_page->IsDirty(), false);
  }
  // Set sync. writeback.
  op.bSync = true;
  // Request writeback for dirty Pages, but there is no Page meeting op.if_page.
  ASSERT_EQ(file.Writeback(op), 0UL);
  // Every writeback Page has been already flushed since op.bSync is set.
  ASSERT_EQ(fs_->GetSuperblockInfo().GetPageCount(CountType::kWriteback), 0);
  ASSERT_EQ(fs_->GetSuperblockInfo().GetPageCount(CountType::kDirtyData), 1);

  key = 1;
  // Set async. writeback.
  op.bSync = false;
  // Now, 2nd Page meets op.if_page.
  ASSERT_EQ(file.Writeback(op), 1UL);
  ASSERT_EQ(file.GetDirtyPageCount(), 0U);
  ASSERT_EQ(fs_->GetSuperblockInfo().GetPageCount(CountType::kDirtyData), 0);
  // Set sync. writeback.
  op.bSync = true;
  // No dirty Pages to be written.
  // All writeback Pages should be clean.
  ASSERT_EQ(file.Writeback(op), 0UL);
  ASSERT_EQ(fs_->GetSuperblockInfo().GetPageCount(CountType::kWriteback), 0);

  // The Pages should be kept but not uptodate since kernel can evict any clean pages.
  {
    fbl::RefPtr<Page> page1, page2;
    ASSERT_EQ(file.FindPage(0, &page1), ZX_OK);
    ASSERT_EQ(file.FindPage(1, &page2), ZX_OK);
    ASSERT_EQ(LockedPage(std::move(page1))->IsUptodate(), false);
    ASSERT_EQ(LockedPage(std::move(page2))->IsUptodate(), false);
  }

  // Invalidate pages
  file.InvalidatePages();
  {
    fbl::RefPtr<Page> page1, page2;
    ASSERT_EQ(file.FindPage(0, &page1), ZX_ERR_NOT_FOUND);
    ASSERT_EQ(file.FindPage(1, &page2), ZX_ERR_NOT_FOUND);
  }
}

TEST_F(FileCacheTest, Recycle) {
  char buf[kPageSize];
  auto &file = vnode<File>();
  FileTester::AppendToFile(&file, buf, kPageSize);

  Page *raw_page;
  {
    LockedPage locked_page = GetPage(0);
    ASSERT_EQ(locked_page->IsDirty(), true);
    raw_page = locked_page.get();
  }

  // Remove |raw_page| from the list to invoke Page::fbl_recycle().
  WritebackOperation op = {.bSync = true, .bReleasePages = false};
  file.Writeback(op);
  ASSERT_FALSE(raw_page->IsDirty());

  // raw_page should be inacive and keep a reference in the tree after Page::fbl_recycle().
  ASSERT_FALSE(raw_page->IsActive());
  ASSERT_TRUE(raw_page->InTreeContainer());
  fbl::RefPtr<Page> page = fbl::ImportFromRawPtr(raw_page);
  ASSERT_EQ(page->IsLastReference(), true);
  raw_page = fbl::ExportToRawPtr(&page);

  FileCache &cache = raw_page->GetFileCache();

  LockedPage locked_page = GetPage(0);
  // Test FileCache::GetPage() and FileCache::Downgrade() with multiple threads
  std::thread thread1([&]() {
    int i = 1000;
    while (--i) {
      LockedPage page = GetPage(0);
      ASSERT_EQ(page.get(), raw_page);
    }
  });

  std::thread thread2([&]() {
    int i = 1000;
    while (--i) {
      LockedPage page = GetPage(0);
      ASSERT_EQ(page.get(), raw_page);
    }
  });
  // Start the execution of threads.
  locked_page.reset();

  thread1.join();
  thread2.join();

  cache.InvalidatePages();

  // Run FileCache::Downgrade() and FileCache::Reset() on different threads.
  std::thread thread_get_page([&]() {
    bool bStop = false;
    std::thread thread_reset([&]() {
      while (!bStop) {
        cache.Reset();
      }
    });

    int i = 1000;
    while (--i) {
      LockedPage page = GetPage(0);
      ASSERT_EQ(page->IsUptodate(), false);
    }
    bStop = true;
    thread_reset.join();
  });

  thread_get_page.join();
}

TEST_F(FileCacheTest, GetPages) {
  constexpr uint32_t kTestNum = 10;
  constexpr uint32_t kTestEndNum = kTestNum * 2;
  char buf[kPageSize * kTestNum];
  auto &file = vnode<File>();

  FileTester::AppendToFile(&file, buf, static_cast<size_t>(kPageSize) * kTestNum);

  std::vector<LockedPage> locked_pages;
  std::vector<pgoff_t> pg_offsets(kTestEndNum);
  std::iota(pg_offsets.begin(), pg_offsets.end(), 0);
  {
    auto pages_or = file.GrabCachePages(pg_offsets);
    ASSERT_TRUE(pages_or.is_ok());
    for (size_t i = 0; i < kTestNum; ++i) {
      ASSERT_EQ(pages_or.value()[i]->IsDirty(), true);
    }
    for (size_t i = kTestNum; i < kTestEndNum; ++i) {
      ASSERT_EQ(pages_or.value()[i]->IsDirty(), false);
    }
    locked_pages = std::move(pages_or.value());
  }

  auto task = [&]() {
    int i = 1000;
    while (--i) {
      auto pages_or = file.GrabCachePages(pg_offsets);
      ASSERT_TRUE(pages_or.is_ok());
      for (size_t i = 0; i < kTestNum; ++i) {
        ASSERT_EQ(pages_or.value()[i]->IsDirty(), true);
      }
      for (size_t i = kTestNum; i < kTestEndNum; ++i) {
        ASSERT_EQ(pages_or.value()[i]->IsDirty(), false);
      }
    }
  };
  // Test FileCache::GetPages() with multiple threads
  std::thread thread1(task);
  std::thread thread2(task);
  // Start threads.
  for (auto &locked_page : locked_pages) {
    locked_page.reset();
  }
  thread1.join();
  thread2.join();
}

TEST_F(FileCacheTest, Basic) {
  uint8_t buf[kPageSize];
  const uint16_t nblocks = 256;
  auto &file = vnode<File>();

  // All pages should not be uptodated.
  for (uint16_t i = 0; i < nblocks; ++i) {
    LockedPage page = GetPage(i);
    // A newly created page should have kPageUptodate/kPageDirty/kPageWriteback flags clear.
    ASSERT_EQ(page->IsUptodate(), false);
    ASSERT_EQ(page->IsDirty(), false);
    ASSERT_EQ(page->IsWriteback(), false);
  }

  // Append |nblocks| * |kPageSize|.
  // Each block is filled with its block offset.
  for (uint16_t i = 0; i < nblocks; ++i) {
    memset(buf, i, kPageSize);
    FileTester::AppendToFile(&file, buf, kPageSize);
  }

  // All pages should be uptodated and dirty.
  for (uint16_t i = 0; i < nblocks; ++i) {
    memset(buf, i, kPageSize);
    LockedPage page = GetPage(i);
    ASSERT_EQ(page->IsUptodate(), true);
    ASSERT_EQ(page->IsDirty(), true);
    BlockBuffer read_buffer;
    page->Read(read_buffer.get());
    ASSERT_EQ(memcmp(buf, read_buffer.get(), kPageSize), 0);
  }

  // Write out some dirty pages
  WritebackOperation op = {.end = nblocks / 2, .bSync = true};
  file.Writeback(op);

  // Check if each page has a correct dirty flag.
  for (size_t i = 0; i < nblocks; ++i) {
    LockedPage page = GetPage(i);
    if (i < nblocks / 2) {
      ASSERT_EQ(page->IsUptodate(), false);
      ASSERT_EQ(page->IsDirty(), false);
    } else {
      ASSERT_EQ(page->IsUptodate(), true);
      ASSERT_EQ(page->IsDirty(), true);
    }
  }
}

TEST_F(FileCacheTest, Truncate) TA_NO_THREAD_SAFETY_ANALYSIS {
  uint8_t buf[kPageSize];
  const uint16_t nblocks = 256;
  auto &file = vnode<File>();

  // Append |nblocks| * |kPageSize|.
  // Each block is filled with its block offset.
  for (uint16_t i = 0; i < nblocks; ++i) {
    memset(buf, i, kPageSize);
    FileTester::AppendToFile(&file, buf, kPageSize);
  }

  // All pages should be uptodated and dirty.
  for (uint16_t i = 0; i < nblocks; ++i) {
    LockedPage page = GetPage(i);
    ASSERT_EQ(page->IsUptodate(), true);
    ASSERT_EQ(page->IsDirty(), true);
  }

  // Truncate test_vnode to the half.
  pgoff_t start = static_cast<pgoff_t>(nblocks) / 2 * kPageSize;
  file.Truncate(start);

  // Check if each page has correct flags.
  for (size_t i = 0; i < nblocks; ++i) {
    LockedPage page = GetPage(i);
    auto data_blkaddr = file.FindDataBlkAddr(i);
    ASSERT_TRUE(data_blkaddr.is_ok());
    if (i >= start / kPageSize) {
      ASSERT_EQ(page->IsDirty(), false);
      ASSERT_EQ(page->IsUptodate(), false);
      ASSERT_EQ(data_blkaddr.value(), kNullAddr);
    } else {
      ASSERT_EQ(page->IsDirty(), true);
      ASSERT_EQ(page->IsUptodate(), true);
      ASSERT_EQ(data_blkaddr.value(), kNewAddr);
    }
  }

  --start;
  // Punch a hole at start
  file.TruncateHole(start, start + 1, true);

  {
    LockedPage page = GetPage(start);
    auto data_blkaddr = file.FindDataBlkAddr(start);
    ASSERT_TRUE(data_blkaddr.is_ok());
    // |page| for the hole should be invalidated.
    ASSERT_EQ(page->IsDirty(), false);
    ASSERT_EQ(page->IsUptodate(), false);
    ASSERT_EQ(data_blkaddr.value(), kNullAddr);
  }
}

TEST_F(FileCacheTest, LockedPageRelease) {
  auto &file = vnode<File>();
  GetPage(0);

  fbl::RefPtr<Page> page;
  ASSERT_EQ(file.FindPage(0, &page), ZX_OK);

  // Lock and unlock page
  LockedPage locked_page(page);
  fbl::RefPtr<Page> released_page = locked_page.release();
  ASSERT_FALSE(locked_page);
  // Check its lock is free
  locked_page = LockedPage(released_page);
  ASSERT_EQ(page, released_page);
}

TEST_F(FileCacheTest, Redirty) {
  char buf[kPageSize];
  FileTester::AppendToFile(&vnode<File>(), buf, kPageSize);
  fbl::RefPtr<Page> page;
  ASSERT_EQ(vnode<File>().FindPage(0, &page), ZX_OK);
  ASSERT_TRUE(page->IsDirty());
  WritebackOperation op = {.bSync = true};
  vnode<File>().Writeback(op);
  ASSERT_FALSE(page->IsDirty());
  auto hook = [](const block_fifo_request_t &_req, const zx::vmo *_vmo) { return ZX_ERR_IO; };
  LockedPage(page).SetDirty();
  ASSERT_TRUE(page->IsDirty());
  DeviceTester::SetHook(fs_.get(), hook);
  vnode<File>().Writeback(op);
  ASSERT_TRUE(page->IsDirty());
  DeviceTester::SetHook(fs_.get(), nullptr);
  vnode<File>().Writeback(op);
  ASSERT_FALSE(page->IsDirty());
  ASSERT_FALSE(fs_->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag));
}

}  // namespace
}  // namespace f2fs
