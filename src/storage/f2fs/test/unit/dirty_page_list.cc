// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/lib/storage/block_client/cpp/fake_block_device.h"
#include "src/storage/f2fs/f2fs.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

using DirtyPageListTest = SingleFileTest;

TEST_F(DirtyPageListTest, AddAndRemoveDirtyPage) {
  File *vn = &vnode<File>();
  ASSERT_EQ(vn->GetDirtyPageList().Size(), 0U);
  {
    LockedPage locked_page;
    vn->GrabCachePage(0, &locked_page);

    // Add dirty Page
    locked_page.SetDirty();
    ASSERT_FALSE(locked_page->IsLastReference());
    ASSERT_EQ(locked_page->IsDirty(), true);
    ASSERT_EQ(locked_page->InTreeContainer(), true);
    ASSERT_EQ(locked_page->InListContainer(), true);
    ASSERT_EQ(vn->GetDirtyPageList().Size(), 1U);
    // Duplicate add is ignored
    ASSERT_EQ(vn->GetDirtyPageList().AddDirty(locked_page).status_value(), ZX_ERR_ALREADY_EXISTS);

    // Remove dirty Page
    ASSERT_TRUE(vn->GetDirtyPageList().RemoveDirty(locked_page) == ZX_OK);
    ASSERT_TRUE(locked_page->IsLastReference());
    locked_page->ClearDirtyForIo();
  }
  ASSERT_EQ(vn->GetDirtyPageList().Size(), 0U);
}

TEST_F(DirtyPageListTest, TakeDirtyPages) {
  File *vn = &vnode<File>();
  char buf[kPageSize];

  // Make dirty Pages
  FileTester::AppendToFile(vn, buf, kPageSize);
  FileTester::AppendToFile(vn, buf, kPageSize);

  ASSERT_EQ(vn->GetDirtyPageList().Size(), 2U);

  for (int i = 0; i < 2; ++i) {
    LockedPage locked_page;
    vn->GrabCachePage(i, &locked_page);
    ASSERT_EQ(locked_page->IsDirty(), true);
    ASSERT_EQ(locked_page->InTreeContainer(), true);
    ASSERT_EQ(locked_page->InListContainer(), true);
  }

  // Hold the lock of the 1st Page.
  LockedPage locked_page;
  vn->GrabCachePage(0, &locked_page);

  // Try to take 2 Pages from the list
  {
    auto pages = vn->GetDirtyPageList().TakePages(2);

    ASSERT_EQ(pages[0]->GetKey(), 1ULL);
    ASSERT_EQ(vn->GetDirtyPageList().Size(), 1U);
    ASSERT_TRUE(pages[0]->ClearDirtyForIo());
  }

  // Release the lock.
  locked_page.reset();

  // Try to take 2 Pages from the list
  {
    auto pages = vn->GetDirtyPageList().TakePages(2);
    ASSERT_EQ(pages[0]->GetKey(), 0ULL);
    ASSERT_EQ(vn->GetDirtyPageList().Size(), 0U);
    ASSERT_TRUE(pages[0]->ClearDirtyForIo());
  }
}

TEST_F(DirtyPageListTest, ResetFileCache) {
  File *vn = &vnode<File>();
  char buf[kPageSize];

  // Make dirty Page
  FileTester::AppendToFile(vn, buf, kPageSize);

  ASSERT_EQ(vn->GetDirtyPageList().Size(), 1U);

  Page *raw_page;
  {
    LockedPage locked_page;
    vn->GrabCachePage(0, &locked_page);
    ASSERT_EQ(locked_page->IsDirty(), true);
    ASSERT_EQ(locked_page->InTreeContainer(), true);
    ASSERT_EQ(locked_page->InListContainer(), true);
    raw_page = locked_page.get();
  }

  raw_page->GetFileCache().Reset();
  ASSERT_EQ(vn->GetDirtyPageList().Size(), 0U);
}

TEST_F(DirtyPageListTest, Invalidate) {
  File *vn = &vnode<File>();
  char buf[kPageSize];

  // Make dirty Page
  FileTester::AppendToFile(vn, buf, kPageSize);

  ASSERT_EQ(vn->GetDirtyPageList().Size(), 1U);

  Page *raw_page;
  {
    LockedPage locked_page;
    vn->GrabCachePage(0, &locked_page);
    ASSERT_EQ(locked_page->IsDirty(), true);
    ASSERT_EQ(locked_page->InTreeContainer(), true);
    ASSERT_EQ(locked_page->InListContainer(), true);
    raw_page = locked_page.get();
  }

  ASSERT_EQ(vn->GetDirtyPageList().Size(), 1U);
  raw_page->GetFileCache().InvalidatePages();
  ASSERT_EQ(vn->GetDirtyPageList().Size(), 0U);
}

}  // namespace
}  // namespace f2fs
