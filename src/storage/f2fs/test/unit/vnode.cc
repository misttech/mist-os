// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <numeric>

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

using VnodeTest = F2fsFakeDevTestFixture;

template <typename T>
void VgetFaultInjetionAndTest(F2fs &fs, Dir &root_dir, std::string_view name, T fault_injection,
                              zx_status_t expected_status) {
  // Create a child
  FileTester::CreateChild(&root_dir, S_IFDIR, name);
  // Log updates on disk
  fs.SyncFs();

  // Check if the child is okay
  fbl::RefPtr<fs::Vnode> vnode;
  FileTester::Lookup(&root_dir, name, &vnode);
  fbl::RefPtr<VnodeF2fs> test_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(vnode));
  nid_t nid = test_vnode->GetKey();
  ASSERT_FALSE(test_vnode->IsDirty());
  ASSERT_EQ(name, test_vnode->GetNameView());

  // fault injection on the inode page after evicting the child from vnode cache
  ASSERT_EQ(fs.GetVCache().Evict(test_vnode.get()), ZX_OK);
  test_vnode->Close();
  test_vnode.reset();
  {
    LockedPage node_page;
    ASSERT_EQ(fs.GetNodeManager().GetNodePage(nid, &node_page), ZX_OK);
    Node *node = node_page->GetAddress<Node>();
    fault_injection(node);
    node_page.SetDirty();
  }

  // Create the child from the faulty node page
  auto vnode_or = fs.GetVnode(nid);
  ASSERT_EQ(vnode_or.status_value(), expected_status);
  if (expected_status == ZX_OK) {
    vnode_or->Close();
  }
}

TEST_F(VnodeTest, Time) {
  std::string dir_name("test");
  zx::result test_fs_vnode = root_dir_->Create(dir_name, fs::CreationType::kDirectory);
  ASSERT_TRUE(test_fs_vnode.is_ok()) << test_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> test_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(test_fs_vnode));

  ASSERT_EQ(test_vnode->GetNameView(), dir_name);

  timespec cur_time;
  clock_gettime(CLOCK_REALTIME, &cur_time);
  ASSERT_LE(zx::duration(test_vnode->GetTime<Timestamps::AccessTime>()), zx::duration(cur_time));
  ASSERT_LE(zx::duration(test_vnode->GetTime<Timestamps::ChangeTime>()), zx::duration(cur_time));
  ASSERT_LE(zx::duration(test_vnode->GetTime<Timestamps::ModificationTime>()),
            zx::duration(cur_time));

  ASSERT_EQ(test_vnode->Close(), ZX_OK);
  test_vnode = nullptr;
}

TEST_F(VnodeTest, Advise) TA_NO_THREAD_SAFETY_ANALYSIS {
  std::string dir_name("test");
  zx::result test_fs_vnode = root_dir_->Create(dir_name, fs::CreationType::kDirectory);
  ASSERT_TRUE(test_fs_vnode.is_ok()) << test_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> test_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(test_fs_vnode));
  Dir *test_dir_ptr = static_cast<Dir *>(test_vnode.get());

  ASSERT_EQ(test_vnode->GetNameView(), dir_name);

  FileTester::CreateChild(test_dir_ptr, S_IFDIR, "f2fs_lower_case.avi");
  fbl::RefPtr<fs::Vnode> file_fs_vnode;
  FileTester::Lookup(test_dir_ptr, "f2fs_lower_case.avi", &file_fs_vnode);
  fbl::RefPtr<VnodeF2fs> file_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(file_fs_vnode));

  ASSERT_FALSE(file_vnode->IsAdviseSet(FAdvise::kCold));
  ASSERT_EQ(file_vnode->Close(), ZX_OK);

  FileTester::CreateChild(test_dir_ptr, S_IFDIR, "f2fs_upper_case.AVI");
  FileTester::Lookup(test_dir_ptr, "f2fs_upper_case.AVI", &file_fs_vnode);
  file_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(file_fs_vnode));

  ASSERT_FALSE(file_vnode->IsAdviseSet(FAdvise::kCold));

  file_vnode->SetColdFile();
  ASSERT_TRUE(file_vnode->IsAdviseSet(FAdvise::kCold));

  file_vnode->ClearAdvise(FAdvise::kCold);
  ASSERT_FALSE(file_vnode->IsAdviseSet(FAdvise::kCold));

  ASSERT_EQ(file_vnode->Close(), ZX_OK);
  file_vnode = nullptr;
  ASSERT_EQ(test_vnode->Close(), ZX_OK);
  test_vnode = nullptr;
}

TEST_F(VnodeTest, EmptyOverridenMethods) {
  char buf[kPageSize];
  size_t out, end;
  zx::vmo vmo;
  ASSERT_EQ(root_dir_->Read(buf, 0, kPageSize, &out), ZX_ERR_NOT_SUPPORTED);
  ASSERT_EQ(root_dir_->Write(buf, 0, kPageSize, &out), ZX_ERR_NOT_SUPPORTED);
  ASSERT_EQ(root_dir_->Append(buf, kPageSize, &end, &out), ZX_ERR_NOT_SUPPORTED);
  ASSERT_EQ(root_dir_->Truncate(0), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(VnodeTest, Mode) {
  zx::result dir_fs_vnode = root_dir_->Create("test_dir", fs::CreationType::kDirectory);
  ASSERT_TRUE(dir_fs_vnode.is_ok()) << dir_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> dir_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(dir_fs_vnode));

  ASSERT_TRUE(S_ISDIR(dir_vnode->GetMode()));
  ASSERT_EQ(dir_vnode->IsDir(), true);
  ASSERT_EQ(dir_vnode->IsReg(), false);
  ASSERT_EQ(dir_vnode->IsLink(), false);
  ASSERT_EQ(dir_vnode->IsChr(), false);
  ASSERT_EQ(dir_vnode->IsBlk(), false);
  ASSERT_EQ(dir_vnode->IsSock(), false);
  ASSERT_EQ(dir_vnode->IsFifo(), false);

  ASSERT_EQ(dir_vnode->Close(), ZX_OK);
  dir_vnode = nullptr;

  zx::result file_fs_vnode = root_dir_->Create("test_file", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> file_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  ASSERT_TRUE(S_ISREG(file_vnode->GetMode()));
  ASSERT_EQ(file_vnode->IsDir(), false);
  ASSERT_EQ(file_vnode->IsReg(), true);
  ASSERT_EQ(file_vnode->IsLink(), false);
  ASSERT_EQ(file_vnode->IsChr(), false);
  ASSERT_EQ(file_vnode->IsBlk(), false);
  ASSERT_EQ(file_vnode->IsSock(), false);
  ASSERT_EQ(file_vnode->IsFifo(), false);

  ASSERT_EQ(file_vnode->Close(), ZX_OK);
  file_vnode = nullptr;
}

TEST_F(VnodeTest, WriteInode) {
  fbl::RefPtr<VnodeF2fs> test_vnode;
  // 1. Check node ino exception
  ASSERT_EQ(fs_->GetVnode(fs_->GetSuperblockInfo().GetNodeIno()).status_value(), ZX_ERR_NOT_FOUND);

  // 2. Check GetNodePage() exception
  FileTester::CreateChild(root_dir_.get(), S_IFDIR, "write_inode_dir");
  fbl::RefPtr<fs::Vnode> dir_raw_vnode;
  FileTester::Lookup(root_dir_.get(), "write_inode_dir", &dir_raw_vnode);
  test_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(dir_raw_vnode));

  // 3. Write inode
  ASSERT_TRUE(test_vnode->IsDirty());
  fs_->SyncFs();
  ASSERT_FALSE(test_vnode->IsDirty());

  ASSERT_EQ(test_vnode->Close(), ZX_OK);
  test_vnode = nullptr;
}

TEST_F(VnodeTest, GetVnodeExceptionCase) {
  DisableFsck();

  fbl::RefPtr<VnodeF2fs> test_vnode;
  NodeManager &node_manager = fs_->GetNodeManager();
  nid_t nid;

  // 1. Check GetVnode() GetNodePage() exception
  auto nid_or = node_manager.AllocNid();
  ASSERT_TRUE(nid_or.is_ok());
  nid = *nid_or;
  ASSERT_EQ(fs_->GetVnode(nid).status_value(), ZX_ERR_NOT_FOUND);

  // 2. Check GetVnode() GetNlink() exception
  auto nlink_fault_inject = [](Node *node) { node->i.i_links = 0; };
  VgetFaultInjetionAndTest(*fs_, *root_dir_, "nlink_dir", nlink_fault_inject, ZX_ERR_NOT_FOUND);
}

TEST_F(VnodeTest, SetAttributes) {
  zx::result dir_fs_vnode = root_dir_->Create("test_dir", fs::CreationType::kDirectory);
  ASSERT_TRUE(dir_fs_vnode.is_ok()) << dir_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> dir_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(dir_fs_vnode));

  ASSERT_EQ(dir_vnode->UpdateAttributes({}), zx::ok());
  ASSERT_EQ(dir_vnode->UpdateAttributes(
                fs::VnodeAttributesUpdate{.creation_time = 1, .modification_time = 1}),
            zx::ok());

  ASSERT_EQ(dir_vnode->Close(), ZX_OK);
  dir_vnode = nullptr;
}

TEST_F(VnodeTest, TruncateExceptionCase) TA_NO_THREAD_SAFETY_ANALYSIS {
  zx::result file_fs_vnode = root_dir_->Create("test_file", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> file_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  // 1. Check TruncateBlocks() exception
  file_vnode->SetSize(1);
  ASSERT_EQ(file_vnode->TruncateBlocks(1), ZX_OK);
  ASSERT_EQ(file_vnode->GetSize(), 1UL);

  const pgoff_t direct_index = 1;
  const pgoff_t direct_blks = kAddrsPerBlock;
  const pgoff_t indirect_blks = static_cast<const pgoff_t>(kAddrsPerBlock) * kNidsPerBlock;
  const pgoff_t indirect_index_lv1 = direct_index + kAddrsPerInode;
  const pgoff_t indirect_index_lv2 = indirect_index_lv1 + direct_blks * 2;
  const pgoff_t indirect_index_lv3 = indirect_index_lv2 + indirect_blks * 2;
  pgoff_t indirect_index_invalid_lv4 = indirect_index_lv3 + indirect_blks * kNidsPerBlock;
  uint32_t blocksize = fs_->GetSuperblockInfo().GetBlocksize();
  uint64_t invalid_size = indirect_index_invalid_lv4 * blocksize;

  file_vnode->SetSize(invalid_size);
  ASSERT_EQ(file_vnode->TruncateBlocks(invalid_size), ZX_ERR_NOT_FOUND);
  ASSERT_EQ(file_vnode->GetSize(), invalid_size);

  // 2. Check TruncateHole() exception
  file_vnode->SetSize(invalid_size);
  ASSERT_EQ(file_vnode->TruncateHole(invalid_size, invalid_size + 1), ZX_OK);
  ASSERT_EQ(file_vnode->GetSize(), invalid_size);

  ASSERT_EQ(file_vnode->Close(), ZX_OK);
  file_vnode = nullptr;

  // 3. Check TruncateToSize() exception
  zx::result block_fs_vnode = root_dir_->CreateWithMode("test_block", S_IFBLK);
  ASSERT_TRUE(block_fs_vnode.is_ok()) << block_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> block_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(block_fs_vnode));
  uint64_t block_size = block_vnode->GetSize();
  block_vnode->TruncateToSize();
  ASSERT_EQ(block_vnode->GetSize(), block_size);

  ASSERT_EQ(block_vnode->Close(), ZX_OK);
  block_vnode = nullptr;
}

TEST_F(VnodeTest, SyncFile) {
  zx::result file_fs_vnode = root_dir_->Create("test_file", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> file_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  // 1. Check need_cp
  uint64_t pre_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  fs_->GetSuperblockInfo().ClearOpt(MountOption::kDisableRollForward);
  file_vnode->SetDirty();
  ASSERT_EQ(file_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);
  fs_->GetSuperblockInfo().SetOpt(MountOption::kDisableRollForward);

  // 2. Check vnode is clean
  pre_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  file_vnode->ClearDirty();
  ASSERT_EQ(file_vnode->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 3. Check kNeedCp
  pre_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  file_vnode->SetFlag(InodeInfoFlag::kNeedCp);
  file_vnode->SetDirty();
  ASSERT_EQ(file_vnode->SyncFile(false), ZX_OK);
  ASSERT_FALSE(file_vnode->TestFlag(InodeInfoFlag::kNeedCp));
  curr_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  // 4. Check SpaceForRollForward()
  pre_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  block_t temp_user_block_count = fs_->GetSuperblockInfo().GetTotalBlockCount();
  fs_->GetSuperblockInfo().SetTotalBlockCount(0);
  file_vnode->SetDirty();
  ASSERT_EQ(file_vnode->SyncFile(false), ZX_OK);
  ASSERT_FALSE(file_vnode->TestFlag(InodeInfoFlag::kNeedCp));
  curr_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);
  fs_->GetSuperblockInfo().SetTotalBlockCount(temp_user_block_count);

  ASSERT_EQ(file_vnode->Close(), ZX_OK);
  file_vnode = nullptr;
}

TEST_F(VnodeTest, GrabLockedPages) {
  zx::result file_fs_vnode = root_dir_->Create("test_file", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> file_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  constexpr pgoff_t kStartOffset = 0;
  constexpr pgoff_t kEndOffset = 1000;

  {
    auto pages_or = file_vnode->GrabLockedPages(kStartOffset, kEndOffset);
    ASSERT_TRUE(pages_or.is_ok());
    for (pgoff_t i = kStartOffset; i < kEndOffset; ++i) {
      LockedPage locked_page = std::move(pages_or.value()[i]);
      auto unlocked_page = locked_page.release();
      ASSERT_EQ(file_vnode->GrabLockedPage(i, &locked_page), ZX_OK);
      ASSERT_EQ(locked_page.get(), unlocked_page.get());
    }
  }

  // Test with holes
  {
    std::vector<pgoff_t> pg_offsets(kEndOffset - kStartOffset);
    std::iota(pg_offsets.begin(), pg_offsets.end(), kStartOffset);
    for (size_t i = 0; i < pg_offsets.size(); i += 2) {
      pg_offsets[i] = kInvalidPageOffset;
    }
    auto pages_or = file_vnode->GrabLockedPages(pg_offsets);
    ASSERT_TRUE(pages_or.is_ok());
    for (size_t i = 0; i < pg_offsets.size(); ++i) {
      if (pg_offsets[i] == kInvalidPageOffset) {
        ASSERT_FALSE(pages_or.value()[i]);
      } else {
        LockedPage locked_page = std::move(pages_or.value()[i]);
        auto unlocked_page = locked_page.release();
        ASSERT_EQ(file_vnode->GrabLockedPage(pg_offsets[i], &locked_page), ZX_OK);
        ASSERT_EQ(locked_page.get(), unlocked_page.get());
      }
    }
  }

  ASSERT_EQ(file_vnode->Close(), ZX_OK);
  file_vnode = nullptr;
}

void CheckDataPages(LockedPagesAndAddrs &address_and_pages, pgoff_t start_offset,
                    pgoff_t end_offset, std::set<block_t> removed_pages) {
  for (uint32_t offset = 0; offset < end_offset - start_offset; ++offset) {
    if (removed_pages.find(offset) != removed_pages.end()) {
      ASSERT_EQ(address_and_pages.block_addrs[offset], kNullAddr);
      continue;
    }
    uint32_t read_offset = 0;
    ASSERT_EQ(address_and_pages.pages[offset]->Read(&read_offset, 0, sizeof(uint32_t)), ZX_OK);
    ASSERT_EQ(read_offset, offset);
  }
}

TEST_F(VnodeTest, FindDataBlockAddrsAndPages) {
  zx::result file_fs_vnode = root_dir_->Create("test_file", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<File> file = fbl::RefPtr<File>::Downcast(*std::move(file_fs_vnode));

  constexpr pgoff_t kStartOffset = 0;
  constexpr pgoff_t kEndOffset = 1000;
  constexpr pgoff_t kMidOffset = kEndOffset / 2;
  constexpr pgoff_t kPageCount = kEndOffset - kStartOffset;
  uint32_t page_count = kPageCount;
  std::set<block_t> removed_pages;

  // Get null block address
  {
    auto addrs_and_pages_or = file->FindDataBlockAddrsAndPages(kStartOffset, kEndOffset);
    ASSERT_TRUE(addrs_and_pages_or.is_ok());
    ASSERT_EQ(addrs_and_pages_or->block_addrs.size(), page_count);
    ASSERT_EQ(addrs_and_pages_or->pages.size(), page_count);
  }

  // Get valid block address
  {
    uint32_t buf[kPageSize / sizeof(uint32_t)];
    for (uint32_t i = 0; i < static_cast<uint32_t>(kPageCount); ++i) {
      buf[0] = i;
      FileTester::AppendToFile(file.get(), buf, kPageSize);
    }
    file->SyncFile(false);

    auto addrs_and_pages_or = file->FindDataBlockAddrsAndPages(kStartOffset, kEndOffset);
    ASSERT_TRUE(addrs_and_pages_or.is_ok());
    ASSERT_EQ(addrs_and_pages_or->block_addrs.size(), page_count);
    ASSERT_EQ(addrs_and_pages_or->pages.size(), page_count);
    CheckDataPages(addrs_and_pages_or.value(), kStartOffset, kEndOffset, removed_pages);
  }

  // Punch a hole at start
  {
    file->TruncateHole(kStartOffset, kStartOffset + 1);
    removed_pages.insert(kStartOffset);

    auto addrs_and_pages_or = file->FindDataBlockAddrsAndPages(kStartOffset, kEndOffset);
    ASSERT_TRUE(addrs_and_pages_or.is_ok());
    ASSERT_EQ(addrs_and_pages_or->block_addrs.size(), page_count);
    ASSERT_EQ(addrs_and_pages_or->pages.size(), page_count);
    CheckDataPages(addrs_and_pages_or.value(), kStartOffset, kEndOffset, removed_pages);
  }

  // Punch a hole at end
  {
    file->TruncateHole(kEndOffset - 1, kEndOffset);
    removed_pages.insert(kEndOffset - 1);

    auto addrs_and_pages_or = file->FindDataBlockAddrsAndPages(kStartOffset, kEndOffset);
    ASSERT_TRUE(addrs_and_pages_or.is_ok());
    ASSERT_EQ(addrs_and_pages_or->block_addrs.size(), page_count);
    ASSERT_EQ(addrs_and_pages_or->pages.size(), page_count);
    CheckDataPages(addrs_and_pages_or.value(), kStartOffset, kEndOffset, removed_pages);
  }

  // Punch holes at middle
  {
    constexpr uint32_t kPunchHoles = 10;
    file->TruncateHole(kMidOffset, kMidOffset + kPunchHoles);
    for (uint32_t i = 0; i < kPunchHoles; ++i) {
      removed_pages.insert(kMidOffset + i);
    }

    auto addrs_and_pages_or = file->FindDataBlockAddrsAndPages(kStartOffset, kEndOffset);
    ASSERT_TRUE(addrs_and_pages_or.is_ok());
    ASSERT_EQ(addrs_and_pages_or->block_addrs.size(), page_count);
    ASSERT_EQ(addrs_and_pages_or->pages.size(), page_count);
    CheckDataPages(addrs_and_pages_or.value(), kStartOffset, kEndOffset, removed_pages);
  }

  ASSERT_EQ(file->Close(), ZX_OK);
  file = nullptr;
}

}  // namespace
}  // namespace f2fs
