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

class VnodeTest : public F2fsFakeDevTestFixture {
 public:
  VnodeTest()
      : F2fsFakeDevTestFixture(TestOptions{
            .mount_options = {{MountOption::kInlineDentry, false}},
        }) {}
};

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

  // For other fsync tests, see FsyncRecoveryTest.
  // 1. Check vnode is clean
  uint64_t pre_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  file_vnode->ClearDirty();
  ASSERT_EQ(file_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 2. Check kNeedCp
  pre_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  file_vnode->SetFlag(InodeInfoFlag::kNeedCp);
  file_vnode->SetDirty();
  ASSERT_EQ(file_vnode->SyncFile(false), ZX_OK);
  ASSERT_FALSE(file_vnode->TestFlag(InodeInfoFlag::kNeedCp));
  curr_checkpoint_ver = fs_->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  // 3. Check SpaceForRollForward()
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

TEST_F(VnodeTest, GetLockedDataPages) TA_NO_THREAD_SAFETY_ANALYSIS {
  zx::result file_fs_vnode = root_dir_->Create("test_dir", fs::CreationType::kDirectory);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<Dir> dir = fbl::RefPtr<Dir>::Downcast(*std::move(file_fs_vnode));

  constexpr pgoff_t kStartOffset = 1;
  constexpr pgoff_t kPageCount = 1000;
  constexpr pgoff_t kEndOffset = kStartOffset + kPageCount;
  constexpr pgoff_t kMidOffset = kEndOffset / 2;
  uint32_t page_count = kPageCount;
  constexpr uint32_t kPunchHoles = 10;

  // Get null block address
  {
    zx::result pages_or = dir->GetLockedDataPages(kStartOffset, kPageCount);
    ASSERT_TRUE(pages_or.is_ok());
    ASSERT_EQ(pages_or->size(), kPageCount);
    for (auto &page : *pages_or) {
      ASSERT_FALSE(page);
    }
  }

  // Make dirty pages
  {
    for (size_t i = kStartOffset; i < kEndOffset; ++i) {
      LockedPage page;
      ASSERT_EQ(dir->GetNewDataPage(i, true, &page), ZX_OK);
      page.SetDirty();
    }
  }

  // Cache hit case
  {
    zx::result pages_or = dir->GetLockedDataPages(kStartOffset, kPageCount);
    ASSERT_TRUE(pages_or.is_ok());
    ASSERT_EQ(pages_or->size(), page_count);
  }

  // Writeback dirty pages
  fs_->SyncFs();

  // Punch holes at middle
  dir->TruncateHole(kMidOffset, kMidOffset + kPunchHoles);

  // Test cache miss/hit cases
  for (size_t test = 0; test < 2; ++test) {
    zx::result pages_or = dir->GetLockedDataPages(kStartOffset, kPageCount);
    ASSERT_TRUE(pages_or.is_ok());
    ASSERT_EQ(pages_or->size(), page_count);
    auto page = pages_or->begin();
    for (size_t i = kStartOffset; i < kEndOffset; ++i, ++page) {
      if (i >= kMidOffset && i < kMidOffset + kPunchHoles) {
        ASSERT_FALSE(*page);
      } else {
        ASSERT_TRUE(*page);
        ASSERT_EQ((*page)->GetKey(), i);
      }
    }
    pages_or->clear();
    dir->GetFileCache().InvalidatePages(kMidOffset);
  }
  ASSERT_EQ(dir->Close(), ZX_OK);
}

}  // namespace
}  // namespace f2fs
