// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <unordered_set>

#include <gtest/gtest.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {
class BitmapTest : public F2fsFakeDevTestFixture {
 public:
  BitmapTest()
      : F2fsFakeDevTestFixture(TestOptions{
            .block_count = 100 * 1024 * 1024 / kDefaultSectorSize,
        }) {}

  zx::result<fbl::RefPtr<VnodeF2fs>> Create(std::string name, bool is_file = true) {
    fbl::RefPtr<fs::Vnode> vnode;
    umode_t mode = is_file ? S_IFREG : S_IFDIR;
    if (zx_status_t status = root_dir_->Create(name, mode, &vnode); status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(fbl::RefPtr<VnodeF2fs>::Downcast(std::move(vnode)));
  }
};

TEST_F(BitmapTest, GetBitmap) {
  auto test_file = Create("file");
  auto test_dir1 = Create("dir1", false);
  auto test_dir2 = Create("dir2", false);

  ASSERT_TRUE(test_file.is_ok());
  ASSERT_TRUE(test_dir1.is_ok());
  ASSERT_TRUE(test_dir2.is_ok());

  LockedPage dir_page1, file_page, dir_page2, dir1_node_page;
  ASSERT_EQ(test_dir1->GrabCachePage(0, &dir_page1), ZX_OK);
  ASSERT_EQ(test_dir2->GrabCachePage(0, &dir_page2), ZX_OK);
  ASSERT_EQ(test_file->GrabCachePage(0, &file_page), ZX_OK);

  auto bits = test_file->GetBitmap(file_page.CopyRefPtr());
  ASSERT_EQ(bits.status_value(), ZX_ERR_NOT_SUPPORTED);

  bits = test_dir1->GetBitmap(dir_page2.CopyRefPtr());
  ASSERT_EQ(bits.status_value(), ZX_ERR_INVALID_ARGS);

  bits = test_dir1->GetBitmap(dir_page1.CopyRefPtr());
  ASSERT_EQ(bits.status_value(), ZX_ERR_INVALID_ARGS);

  test_dir1->ClearFlag(InodeInfoFlag::kInlineDentry);
  bits = test_dir1->GetBitmap(dir_page1.CopyRefPtr());
  ASSERT_TRUE(bits.is_ok());

  test_dir1->SetFlag(InodeInfoFlag::kInlineDentry);
  fs_->GetNodeManager().GetNodePage(test_dir1->GetKey(), &dir1_node_page);
  bits = test_dir1->GetBitmap(dir1_node_page.CopyRefPtr());
  ASSERT_TRUE(bits.is_ok());

  test_file->Close();
  test_dir1->Close();
  test_dir2->Close();
}

TEST_F(BitmapTest, BasicOp) {
  auto file = Create("dir", false);
  ASSERT_TRUE(file.is_ok());

  LockedPage page;
  ASSERT_EQ(file->GrabCachePage(0, &page), ZX_OK);
  size_t size = GetBitSize(page->Size());
  size_t off = size;
  PageBitmap bits(page.CopyRefPtr(), page->GetAddress(), off);
  ASSERT_EQ(bits.Set(off), false);
  ASSERT_EQ(bits.Test(off), false);
  ASSERT_EQ(bits.Clear(off), false);
  ASSERT_EQ(bits.FindNextZeroBit(0), 0U);
  ASSERT_EQ(bits.FindNextZeroBit(off), off);
  ASSERT_EQ(bits.FindNextBit(0), off);
  ASSERT_EQ(bits.FindNextBit(off), off);

  --off;
  ASSERT_EQ(bits.Set(off), false);
  ASSERT_EQ(bits.Test(off), true);
  ASSERT_EQ(bits.FindNextZeroBit(0), 0U);
  ASSERT_EQ(bits.FindNextBit(0), off);
  ASSERT_EQ(bits.Clear(off), true);

  size_t msb_first = off - (off & kLastNodeMask) + (7 - off & kLastNodeMask);
  ASSERT_EQ(ToMsbFirst(off), msb_first);

  ASSERT_EQ(bits.Set(ToMsbFirst(0)), false);
  ASSERT_EQ(bits.Test(7), true);
  ASSERT_EQ(bits.Test(0), false);

  file->Close();
}

}  // namespace
}  // namespace f2fs
