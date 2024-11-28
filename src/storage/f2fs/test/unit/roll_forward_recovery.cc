// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>
#include <string_view>
#include <unordered_set>

#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

zx_status_t CheckDataPage(F2fs *fs, pgoff_t data_blkaddr, uint32_t index) {
  zx_status_t ret = ZX_ERR_INVALID_ARGS;
  LockedPage page;
  if (ret = fs->GetMetaPage(data_blkaddr, &page); ret != ZX_OK) {
    return ret;
  }
  if (*static_cast<uint32_t *>(page->GetAddress()) == index) {
    ret = ZX_OK;
  }
  return ret;
}

block_t StartBidxOfNodeWithoutVnode(NodePage &node_page) {
  constexpr uint32_t kOfsInode = 0;
  constexpr uint32_t kOfsDirectNode2 = 2;
  constexpr uint32_t kOfsIndirectNode1 = 3;
  constexpr uint32_t kOfsIndirectNode2 = 4 + kNidsPerBlock;
  constexpr uint32_t kOfsDoubleIndirectNode = 5 + 2 * kNidsPerBlock;
  uint32_t node_ofs = node_page.OfsOfNode(), NumOfIndirectNodes = 0;

  if (node_ofs == kOfsInode) {
    return 0;
  } else if (node_ofs <= kOfsDirectNode2) {
    NumOfIndirectNodes = 0;
  } else if (node_ofs >= kOfsIndirectNode1 && node_ofs < kOfsIndirectNode2) {
    NumOfIndirectNodes = 1;
  } else if (node_ofs >= kOfsIndirectNode2 && node_ofs < kOfsDoubleIndirectNode) {
    NumOfIndirectNodes = 2;
  } else {
    NumOfIndirectNodes = (node_ofs - kOfsDoubleIndirectNode - 2) / (kNidsPerBlock + 1);
  }

  uint32_t bidx = node_ofs - NumOfIndirectNodes - 1;
  // Since the test does not use InlineXattr, Use |kAddrsPerInode| value instead of
  // |VnodeF2fs::GetAddrsPerInode| function.
  return (kAddrsPerInode + safemath::CheckMul(bidx, kAddrsPerBlock)).ValueOrDie();
}

zx::result<pgoff_t> CheckNodePage(F2fs *fs, NodePage &node_page) {
  uint32_t block_count = 0, start_index = 0, checked = 0;

  if (node_page.IsInode()) {
    block_count = kAddrsPerInode;
  } else {
    block_count = kAddrsPerBlock;
  }

  start_index = StartBidxOfNodeWithoutVnode(node_page);

  for (uint32_t index = 0; index < block_count; ++index) {
    block_t data_blkaddr = node_page.GetBlockAddr(index);
    if (data_blkaddr == kNullAddr) {
      continue;
    }
    if (CheckDataPage(fs, data_blkaddr, safemath::checked_cast<uint32_t>(start_index + index)) !=
        ZX_OK) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    ++checked;
  }
  return zx::ok(checked);
}

zx::result<fbl::RefPtr<VnodeF2fs>> CreateFileAndWritePages(Dir *dir_vnode,
                                                           std::string_view file_name,
                                                           pgoff_t page_count, uint32_t signiture) {
  zx::result file_fs_vnode = dir_vnode->Create(file_name, fs::CreationType::kFile);
  if (file_fs_vnode.is_error()) {
    return file_fs_vnode.take_error();
  }
  fbl::RefPtr<VnodeF2fs> fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));
  File *fsync_file_ptr = static_cast<File *>(fsync_vnode.get());

  // Write a page
  for (uint32_t index = 0; index < page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    for (uint32_t &integer : write_buf) {
      integer = index + signiture;
    }
    FileTester::AppendToFile(fsync_file_ptr, write_buf, PAGE_SIZE);
  }
  return zx::ok(std::move(fsync_vnode));
}

void CheckFsyncedFile(F2fs *fs, ino_t ino, pgoff_t data_page_count, pgoff_t node_page_count) {
  block_t data_blkaddr = fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpointVer(true);
  pgoff_t checked_data_page_count = 0;
  pgoff_t checked_node_page_count = 0;

  while (true) {
    LockedPage page;
    ASSERT_EQ(fs->GetMetaPage(data_blkaddr, &page), ZX_OK);
    NodePage *node_page = &page.GetPage<NodePage>();

    if (curr_checkpoint_ver != node_page->CpverOfNode()) {
      break;
    }

    if (node_page->InoOfNode() == ino) {
      if (node_page_count == ++checked_node_page_count) {
        ASSERT_TRUE(node_page->IsFsyncDnode());
      } else {
        ASSERT_FALSE(node_page->IsFsyncDnode());
      }
      auto result = CheckNodePage(fs, *node_page);
      ASSERT_EQ(result.status_value(), ZX_OK);
      checked_data_page_count += result.value();
    }
    data_blkaddr = node_page->NextBlkaddrOfNode();
  }
  ASSERT_EQ(checked_data_page_count, data_page_count);
  ASSERT_EQ(checked_node_page_count, node_page_count);
}

TEST(FsyncRecoveryTest, FsyncInode) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file and write data pages
  const pgoff_t data_page_count = 1;
  const pgoff_t node_page_count = 1;
  auto ret = CreateFileAndWritePages(root_dir.get(), "fsync_inode_file", data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode = std::move(ret.value());

  // 2. Fsync file
  ino_t fsync_file_ino = fsync_vnode->Ino();
  block_t pre_next_node_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  block_t pre_next_data_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmData);

  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 3. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 4. Remount without roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 1), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 5. Check fsynced inode pages
  block_t curr_next_node_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  ASSERT_EQ(pre_next_node_blkaddr, curr_next_node_blkaddr);
  block_t curr_next_data_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmData);
  ASSERT_EQ(pre_next_data_blkaddr, curr_next_data_blkaddr);

  CheckFsyncedFile(fs.get(), fsync_file_ino, data_page_count, node_page_count);

  // 6. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, FsyncDnode) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file and write data pages to use dnode.
  const pgoff_t data_page_count = kAddrsPerInode + 1;
  const pgoff_t node_page_count = 2;
  auto ret = CreateFileAndWritePages(root_dir.get(), "fsync_dnode_file", data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode = std::move(ret.value());

  // 2. Fsync file
  ino_t fsync_file_ino = fsync_vnode->Ino();
  block_t pre_next_node_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  block_t pre_next_data_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmData);

  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 3. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 4. Remount without roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 1), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 5. Check fsynced inode pages
  block_t curr_next_node_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  ASSERT_EQ(pre_next_node_blkaddr, curr_next_node_blkaddr);
  block_t curr_next_data_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmData);
  ASSERT_EQ(pre_next_data_blkaddr, curr_next_data_blkaddr);

  CheckFsyncedFile(fs.get(), fsync_file_ino, data_page_count, node_page_count);

  // 6. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, FsyncIndirectDnode) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file and write data pages to use indirect dnode.
  const pgoff_t data_page_count = kAddrsPerInode + kAddrsPerBlock * 2 + 1;
  const pgoff_t node_page_count = 4;
  auto ret =
      CreateFileAndWritePages(root_dir.get(), "fsync_indirect_dnode_file", data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode = std::move(ret.value());

  // 2. Fsync file
  ino_t fsync_file_ino = fsync_vnode->Ino();
  block_t pre_next_node_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  block_t pre_next_data_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmData);

  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 3. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 4. Remount without roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 1), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 5. Check fsynced inode pages
  block_t curr_next_node_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode);
  ASSERT_EQ(pre_next_node_blkaddr, curr_next_node_blkaddr);
  block_t curr_next_data_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmData);
  ASSERT_EQ(pre_next_data_blkaddr, curr_next_data_blkaddr);

  CheckFsyncedFile(fs.get(), fsync_file_ino, data_page_count, node_page_count);

  // 6. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, FsyncCheckpoint) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Fsync directory
  zx::result file_fs_vnode = root_dir->Create("fsync_dir", fs::CreationType::kDirectory);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // fsync should trigger checkpoint
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;

  // 2. Fsync Nlink > 1
  file_fs_vnode = root_dir->Create("fsync_file_nlink", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));
  fsync_vnode->IncNlink();

  pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // fsync should trigger checkpoint
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);
  fsync_vnode->DropNlink();
  fsync_vnode->SetDirty();

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;

  // 3. Fsync vnode with kNeedCp flag
  file_fs_vnode = root_dir->Create("fsync_file_need_cp", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));
  fsync_vnode->SetFlag(InodeInfoFlag::kNeedCp);

  pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // fsync should trigger checkpoint
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;

  // 4. Not enough SpaceForRollForward
  file_fs_vnode = root_dir->Create("fsync_file_space_for_roll_forward", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));
  block_t temp_user_block_count = fs->GetSuperblockInfo().GetTotalBlockCount();
  fs->GetSuperblockInfo().SetTotalBlockCount(0);

  pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // fsync should trigger checkpoint
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);
  fs->GetSuperblockInfo().SetTotalBlockCount(temp_user_block_count);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;

  // 5. NeedToSyncDir()
  FileTester::CreateChild(root_dir.get(), S_IFDIR, "parent_dir");
  fbl::RefPtr<fs::Vnode> child_dir_vn;
  FileTester::Lookup(root_dir.get(), "parent_dir", &child_dir_vn);
  file_fs_vnode = child_dir_vn->Create("fsync_file", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // fsync should trigger checkpoint
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(child_dir_vn->Close(), ZX_OK);
  child_dir_vn = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 6. Enable kMountDisableRollForward option
  // Remount without roll-forward recovery
  FileTester::Unmount(std::move(fs), &bc);
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 1), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));
  file_fs_vnode = root_dir->Create("fsync_file_disable_roll_forward", fs::CreationType::kFile);
  ASSERT_TRUE(file_fs_vnode.is_ok()) << file_fs_vnode.status_string();
  fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(file_fs_vnode));

  pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // fsync should trigger checkpoint
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, FsyncRecoveryIndirectDnode) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file and write data pages to use indirect dnode.
  const pgoff_t data_page_count = kAddrsPerInode + kAddrsPerBlock * 2 + 1;
  std::string file_name("recovery_indirect_dnode_file");
  auto ret = CreateFileAndWritePages(root_dir.get(), file_name, data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode = std::move(ret.value());

  // 2. Fsync file
  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 4. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 5. Remount with roll-forward recovery
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  // 6. Check fsynced file
  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  fbl::RefPtr<fs::Vnode> file_fs_vnode;
  FileTester::Lookup(root_dir.get(), file_name, &file_fs_vnode);
  fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(file_fs_vnode));
  File *fsync_file_ptr = static_cast<File *>(fsync_vnode.get());

  ASSERT_EQ(fsync_vnode->GetSize(), data_page_count * PAGE_SIZE);

  for (uint32_t index = 0; index < data_page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(fsync_file_ptr, write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index);
  }

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 7. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, FsyncRecoveryMultipleFiles) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc, kSectorCount100MiB);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file 1
  const pgoff_t data_page_count_1 = kAddrsPerInode + kAddrsPerBlock * 2 + 1;
  uint32_t file_1_signature = 0x111111;
  std::string file_name_1("recovery_file_1");
  auto ret =
      CreateFileAndWritePages(root_dir.get(), file_name_1, data_page_count_1, file_1_signature);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode_1 = std::move(ret.value());

  // 2. Fsync file 1
  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode_1->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 3. Create file 2
  const pgoff_t data_page_count_2 = kAddrsPerInode + kAddrsPerBlock * 2 + 1;
  uint32_t file_2_signature = 0x222222;
  std::string file_name_2("recovery_file_2");
  ret = CreateFileAndWritePages(root_dir.get(), file_name_2, data_page_count_2, file_2_signature);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode_2 = std::move(ret.value());

  // 4. Fsync file 2
  pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(fsync_vnode_2->SyncFile(false), ZX_OK);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  ASSERT_EQ(fsync_vnode_1->Close(), ZX_OK);
  fsync_vnode_1 = nullptr;
  ASSERT_EQ(fsync_vnode_2->Close(), ZX_OK);
  fsync_vnode_2 = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 5. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 6. Remount with roll-forward recovery
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 7. Check fsynced file 1
  fbl::RefPtr<fs::Vnode> file_fs_vnode_1;
  FileTester::Lookup(root_dir.get(), file_name_1, &file_fs_vnode_1);
  fsync_vnode_1 = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(file_fs_vnode_1));
  File *fsync_file_ptr_1 = static_cast<File *>(fsync_vnode_1.get());

  ASSERT_EQ(fsync_vnode_1->GetSize(), data_page_count_1 * PAGE_SIZE);

  for (uint32_t index = 0; index < data_page_count_1; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(fsync_file_ptr_1, write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index + file_1_signature);
  }

  // 8. Check fsynced file 2
  fbl::RefPtr<fs::Vnode> file_fs_vnode_2;
  FileTester::Lookup(root_dir.get(), file_name_2, &file_fs_vnode_2);
  fsync_vnode_2 = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(file_fs_vnode_2));
  File *fsync_file_ptr_2 = static_cast<File *>(fsync_vnode_2.get());

  ASSERT_EQ(fsync_vnode_2->GetSize(), data_page_count_2 * PAGE_SIZE);

  for (uint32_t index = 0; index < data_page_count_2; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(fsync_file_ptr_2, write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index + file_2_signature);
  }

  ASSERT_EQ(fsync_vnode_1->Close(), ZX_OK);
  fsync_vnode_1 = nullptr;
  ASSERT_EQ(fsync_vnode_2->Close(), ZX_OK);
  fsync_vnode_2 = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 9. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, FsyncRecoveryInlineData) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // The inline_data recovery policy is as follows.
  // [prev.] [next] of inline_data flag
  //    o       o  -> 1. recover inline_data
  //    o       x  -> 2. remove inline_data, and then recover data blocks

  // 1. recover inline_data
  // Inline file creation
  std::string inline_file_name("inline");
  zx::result inline_raw_vnode = root_dir->Create(inline_file_name, fs::CreationType::kFile);
  ASSERT_TRUE(inline_raw_vnode.is_ok()) << inline_raw_vnode.status_string();
  fbl::RefPtr<VnodeF2fs> inline_vnode =
      fbl::RefPtr<VnodeF2fs>::Downcast(*std::move(inline_raw_vnode));
  File *inline_file_ptr = static_cast<File *>(inline_vnode.get());
  inline_vnode->SetFlag(InodeInfoFlag::kInlineData);
  FileTester::CheckInlineFile(inline_vnode.get());

  fs->SyncFs();

  // Write until entire inline data space is written
  size_t target_size = inline_file_ptr->MaxInlineData() - 1;
  auto w_buf = std::make_unique<char[]>(inline_file_ptr->MaxInlineData());
  auto r_buf = std::make_unique<char[]>(inline_file_ptr->MaxInlineData());

  for (size_t i = 0; i < inline_file_ptr->MaxInlineData(); ++i) {
    w_buf[i] = static_cast<char>(rand());
  }

  // fill inline data
  FileTester::AppendToInline(inline_file_ptr, w_buf.get(), target_size);
  FileTester::CheckInlineFile(inline_vnode.get());
  ASSERT_EQ(inline_file_ptr->GetSize(), target_size);

  // fsync()
  ASSERT_EQ(inline_vnode->SyncFile(false), ZX_OK);
  ASSERT_EQ(inline_vnode->Close(), ZX_OK);
  inline_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // SPO and remount with roll-forward recovery
  // |inline_vnode| should be recovered with the inline data.
  FileTester::SuddenPowerOff(std::move(fs), &bc);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  fbl::RefPtr<fs::Vnode> lookup_vn;
  FileTester::Lookup(root_dir.get(), inline_file_name, &lookup_vn);
  inline_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(lookup_vn));
  inline_file_ptr = static_cast<File *>(inline_vnode.get());
  FileTester::CheckInlineFile(inline_vnode.get());

  // Files using pager migrate inline data to regular data blocks when they need to handle stream.
  // Thus, |inline_vnode| moves inline data to data block during ReadFromFile().
  FileTester::ReadFromFile(inline_file_ptr, r_buf.get(), target_size, 0);
  ASSERT_EQ(memcmp(r_buf.get(), w_buf.get(), target_size), 0);
  FileTester::CheckNonInlineFile(inline_vnode.get());

  // fsync()
  ASSERT_EQ(inline_file_ptr->GetSize(), target_size);
  ASSERT_EQ(inline_vnode->SyncFile(false), ZX_OK);
  ASSERT_EQ(inline_vnode->Close(), ZX_OK);
  inline_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // SPO and remount with roll-forward recovery
  // |inline_vnode| should be recovered without any inline data.
  FileTester::SuddenPowerOff(std::move(fs), &bc);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  FileTester::Lookup(root_dir.get(), inline_file_name, &lookup_vn);
  inline_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(lookup_vn));
  inline_file_ptr = static_cast<File *>(inline_vnode.get());
  FileTester::CheckNonInlineFile(inline_vnode.get());

  ASSERT_EQ(inline_file_ptr->GetSize(), target_size);
  FileTester::ReadFromFile(inline_file_ptr, r_buf.get(), target_size, 0);
  ASSERT_EQ(memcmp(r_buf.get(), w_buf.get(), target_size), 0);

  ASSERT_EQ(inline_vnode->Close(), ZX_OK);
  inline_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, RecoveryWithoutFsync) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file and write data pages to use indirect dnode.
  const pgoff_t data_page_count = 1;
  std::string file_name("recovery_without_fsync_file");
  auto ret = CreateFileAndWritePages(root_dir.get(), file_name, data_page_count, 0);

  auto fsync_vnode = std::move(ret.value());

  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 2. SPO without fsync
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 3. Remount with roll-forward recovery
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  // 4. Check fsynced file
  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // File not found.
  fbl::RefPtr<fs::Vnode> file_fs_vnode;
  FileTester::Lookup(root_dir.get(), file_name, &file_fs_vnode);
  ASSERT_EQ(file_fs_vnode, nullptr);

  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 5. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, RenameFileWithStrictFsync) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  // This is same scenario of xfstest generic/342
  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create "a"
  FileTester::CreateChild(root_dir.get(), S_IFDIR, "a");
  fbl::RefPtr<fs::Vnode> child_dir_vn;
  FileTester::Lookup(root_dir.get(), "a", &child_dir_vn);
  fbl::RefPtr<Dir> child_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_dir_vn));
  ASSERT_EQ(child_dir->SyncFile(false), ZX_OK);

  // 2. Create "a/foo"
  uint32_t first_signature = 0xa1;
  uint32_t data_page_count = 4;
  auto ret = CreateFileAndWritePages(child_dir.get(), "foo", data_page_count, first_signature);
  ASSERT_TRUE(ret.is_ok());
  fbl::RefPtr<VnodeF2fs> first_foo_vnode = std::move(*ret);
  ASSERT_EQ(first_foo_vnode->SyncFile(false), ZX_OK);

  // 3. Rename "a/foo" to "a/bar"
  FileTester::RenameChild(child_dir, child_dir, "foo", "bar");

  // 4. Create "a/foo"
  uint32_t second_signature = 0xb2;
  ret = CreateFileAndWritePages(child_dir.get(), "foo", data_page_count, second_signature);
  ASSERT_TRUE(ret.is_ok());
  fbl::RefPtr<VnodeF2fs> second_foo_vnode = std::move(*ret);

  // 5. Fsync "a/foo"
  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(second_foo_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should be performed instead of fsync in STRICT mode
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(first_foo_vnode->Close(), ZX_OK);
  first_foo_vnode = nullptr;
  ASSERT_EQ(second_foo_vnode->Close(), ZX_OK);
  second_foo_vnode = nullptr;
  ASSERT_EQ(child_dir->Close(), ZX_OK);
  child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 6. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 7. Remount
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  FileTester::Lookup(root_dir.get(), "a", &child_dir_vn);
  child_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_dir_vn));

  // 8. Find "a/bar"
  fbl::RefPtr<fs::Vnode> first_foo_vn;
  FileTester::Lookup(child_dir.get(), "bar", &first_foo_vn);
  auto first_foo_file = fbl::RefPtr<File>::Downcast(std::move(first_foo_vn));

  // 9. Find "a/foo"
  fbl::RefPtr<fs::Vnode> second_foo_vn;
  FileTester::Lookup(child_dir.get(), "foo", &second_foo_vn);
  auto second_foo_file = fbl::RefPtr<File>::Downcast(std::move(second_foo_vn));

  // 10. Check fsynced file
  ASSERT_EQ(first_foo_file->GetSize(), data_page_count * PAGE_SIZE);
  for (uint32_t index = 0; index < data_page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(first_foo_file.get(), write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index + first_signature);
  }

  ASSERT_EQ(second_foo_file->GetSize(), data_page_count * PAGE_SIZE);
  for (uint32_t index = 0; index < data_page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(second_foo_file.get(), write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index + second_signature);
  }

  ASSERT_EQ(first_foo_file->Close(), ZX_OK);
  first_foo_file = nullptr;
  ASSERT_EQ(second_foo_file->Close(), ZX_OK);
  second_foo_file = nullptr;
  ASSERT_EQ(child_dir->Close(), ZX_OK);
  child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 11. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, RenameFileToOtherDirWithStrictFsync) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create "a"
  FileTester::CreateChild(root_dir.get(), S_IFDIR, "a");
  fbl::RefPtr<fs::Vnode> child_a_dir_vn;
  FileTester::Lookup(root_dir.get(), "a", &child_a_dir_vn);
  fbl::RefPtr<Dir> child_a_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_a_dir_vn));
  ASSERT_EQ(child_a_dir->SyncFile(false), ZX_OK);

  // 1. Create "b"
  FileTester::CreateChild(root_dir.get(), S_IFDIR, "b");
  fbl::RefPtr<fs::Vnode> child_b_dir_vn;
  FileTester::Lookup(root_dir.get(), "b", &child_b_dir_vn);
  fbl::RefPtr<Dir> child_b_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_b_dir_vn));
  ASSERT_EQ(child_b_dir->SyncFile(false), ZX_OK);

  // 2. Create "a/foo"
  uint32_t first_signature = 0xa1;
  uint32_t data_page_count = 4;
  auto ret = CreateFileAndWritePages(child_a_dir.get(), "foo", data_page_count, first_signature);
  ASSERT_TRUE(ret.is_ok());
  fbl::RefPtr<VnodeF2fs> first_foo_vnode = std::move(*ret);
  ASSERT_EQ(first_foo_vnode->SyncFile(false), ZX_OK);

  // 3. Rename "a/foo" to "b/bar"
  FileTester::RenameChild(child_a_dir, child_b_dir, "foo", "bar");

  // 4. Create "a/foo"
  uint32_t second_signature = 0xb2;
  ret = CreateFileAndWritePages(child_a_dir.get(), "foo", data_page_count, second_signature);
  ASSERT_TRUE(ret.is_ok());
  fbl::RefPtr<VnodeF2fs> second_foo_vnode = std::move(*ret);

  // 5. Fsync "a/foo"
  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(second_foo_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should be performed instead of fsync in STRICT mode
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(first_foo_vnode->Close(), ZX_OK);
  first_foo_vnode = nullptr;
  ASSERT_EQ(second_foo_vnode->Close(), ZX_OK);
  second_foo_vnode = nullptr;
  ASSERT_EQ(child_a_dir->Close(), ZX_OK);
  child_a_dir = nullptr;
  ASSERT_EQ(child_b_dir->Close(), ZX_OK);
  child_b_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 6. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 7. Remount
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  FileTester::Lookup(root_dir.get(), "a", &child_a_dir_vn);
  child_a_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_a_dir_vn));

  FileTester::Lookup(root_dir.get(), "b", &child_b_dir_vn);
  child_b_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_b_dir_vn));

  // 8. Find "b/bar"
  fbl::RefPtr<fs::Vnode> first_foo_vn;
  FileTester::Lookup(child_b_dir.get(), "bar", &first_foo_vn);
  auto first_foo_file = fbl::RefPtr<File>::Downcast(std::move(first_foo_vn));

  // 9. Find "a/foo"
  fbl::RefPtr<fs::Vnode> second_foo_vn;
  FileTester::Lookup(child_a_dir.get(), "foo", &second_foo_vn);
  auto second_foo_file = fbl::RefPtr<File>::Downcast(std::move(second_foo_vn));

  // 10. Check fsynced file
  ASSERT_EQ(first_foo_file->GetSize(), data_page_count * PAGE_SIZE);
  for (uint32_t index = 0; index < data_page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(first_foo_file.get(), write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index + first_signature);
  }

  ASSERT_EQ(second_foo_file->GetSize(), data_page_count * PAGE_SIZE);
  for (uint32_t index = 0; index < data_page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(second_foo_file.get(), write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index + second_signature);
  }

  ASSERT_EQ(first_foo_file->Close(), ZX_OK);
  first_foo_file = nullptr;
  ASSERT_EQ(second_foo_file->Close(), ZX_OK);
  second_foo_file = nullptr;
  ASSERT_EQ(child_a_dir->Close(), ZX_OK);
  child_a_dir = nullptr;
  ASSERT_EQ(child_b_dir->Close(), ZX_OK);
  child_b_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 11. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, RenameDirectoryWithStrictFsync) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create "a"
  FileTester::CreateChild(root_dir.get(), S_IFDIR, "a");
  fbl::RefPtr<fs::Vnode> child_dir_vn;
  FileTester::Lookup(root_dir.get(), "a", &child_dir_vn);
  fbl::RefPtr<Dir> child_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_dir_vn));
  ASSERT_EQ(child_dir->SyncFile(false), ZX_OK);

  // 2. Create "a/foo"
  FileTester::CreateChild(child_dir.get(), S_IFDIR, "foo");
  fbl::RefPtr<fs::Vnode> first_foo_vnode;
  FileTester::Lookup(child_dir.get(), "foo", &first_foo_vnode);
  auto first_foo_dir = fbl::RefPtr<Dir>::Downcast(std::move(first_foo_vnode));
  FileTester::CreateChild(first_foo_dir.get(), S_IFREG, "bar_verification_file");
  ASSERT_EQ(first_foo_dir->SyncFile(false), ZX_OK);

  // 3. Rename "a/foo" to "a/bar"
  FileTester::RenameChild(child_dir, child_dir, "foo", "bar");

  // 4. Create "a/foo"
  FileTester::CreateChild(child_dir.get(), S_IFDIR, "foo");
  fbl::RefPtr<fs::Vnode> second_foo_vnode;
  FileTester::Lookup(child_dir.get(), "foo", &second_foo_vnode);
  auto second_foo_dir = fbl::RefPtr<Dir>::Downcast(std::move(second_foo_vnode));
  FileTester::CreateChild(second_foo_dir.get(), S_IFREG, "foo_verification_file");

  // 5. Fsync "a/foo"
  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(second_foo_dir->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should be performed instead of fsync in STRICT mode
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  ASSERT_EQ(first_foo_dir->Close(), ZX_OK);
  first_foo_dir = nullptr;
  ASSERT_EQ(second_foo_dir->Close(), ZX_OK);
  second_foo_dir = nullptr;
  ASSERT_EQ(child_dir->Close(), ZX_OK);
  child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 6. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 7. Remount
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  FileTester::Lookup(root_dir.get(), "a", &child_dir_vn);
  child_dir = fbl::RefPtr<Dir>::Downcast(std::move(child_dir_vn));

  // 8. Find "a/bar"
  fbl::RefPtr<fs::Vnode> first_foo_vn;
  FileTester::Lookup(child_dir.get(), "bar", &first_foo_vn);
  first_foo_dir = fbl::RefPtr<Dir>::Downcast(std::move(first_foo_vn));
  ASSERT_NE(first_foo_dir, nullptr);
  fbl::RefPtr<fs::Vnode> bar_verfication_vn;
  FileTester::Lookup(first_foo_dir.get(), "bar_verification_file", &bar_verfication_vn);
  ASSERT_NE(bar_verfication_vn, nullptr);

  // 9. Find "a/foo"
  fbl::RefPtr<fs::Vnode> second_foo_vn;
  FileTester::Lookup(child_dir.get(), "foo", &second_foo_vn);
  second_foo_dir = fbl::RefPtr<Dir>::Downcast(std::move(second_foo_vn));
  ASSERT_NE(second_foo_dir, nullptr);
  fbl::RefPtr<fs::Vnode> foo_verfication_vn;
  FileTester::Lookup(second_foo_dir.get(), "foo_verification_file", &foo_verfication_vn);
  ASSERT_NE(foo_verfication_vn, nullptr);

  ASSERT_EQ(bar_verfication_vn->Close(), ZX_OK);
  bar_verfication_vn = nullptr;
  ASSERT_EQ(foo_verfication_vn->Close(), ZX_OK);
  foo_verfication_vn = nullptr;
  ASSERT_EQ(first_foo_dir->Close(), ZX_OK);
  first_foo_dir = nullptr;
  ASSERT_EQ(second_foo_dir->Close(), ZX_OK);
  second_foo_dir = nullptr;
  ASSERT_EQ(child_dir->Close(), ZX_OK);
  child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 11. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, AtomicFsync) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc, kSectorCount100MiB);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file and write data pages.
  const pgoff_t data_page_count = kAddrsPerInode + kAddrsPerBlock * 2 + 1;
  std::string valid_file_name("valid_fsync_file");
  auto ret = CreateFileAndWritePages(root_dir.get(), valid_file_name, data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto valid_fsync_vnode = std::move(ret.value());

  std::string invalid_file_name("invalid_fsync_file");
  ret = CreateFileAndWritePages(root_dir.get(), invalid_file_name, data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto invalid_fsync_vnode = std::move(ret.value());

  // 2. Fsync file
  uint64_t pre_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(valid_fsync_vnode->SyncFile(false), ZX_OK);
  ASSERT_EQ(invalid_fsync_vnode->SyncFile(false), ZX_OK);
  uint64_t curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  // Checkpoint should not be performed instead of fsync
  ASSERT_EQ(pre_checkpoint_ver, curr_checkpoint_ver);

  // 3. currupt invalid_fsync_file's last dnode page
  block_t last_dnode_blkaddr =
      fs->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegWarmNode) - 1;
  BlockBuffer<Node> node_block;
  fs->GetBc().Readblk(last_dnode_blkaddr, &node_block);
  ASSERT_EQ(fs->GetSuperblockInfo().GetCheckpointVer(true), LeToCpu(node_block->footer.cp_ver));
  ASSERT_EQ(node_block->footer.ino, invalid_fsync_vnode->Ino());
  uint32_t mask = 1 << static_cast<uint32_t>(BitShift::kFsyncBitShift);
  ASSERT_NE(mask & node_block->footer.flag, 0U);

  uint32_t dummy_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))] = {0};
  fs->GetBc().Writeblk(last_dnode_blkaddr, dummy_buf);

  ASSERT_EQ(valid_fsync_vnode->Close(), ZX_OK);
  valid_fsync_vnode = nullptr;
  ASSERT_EQ(invalid_fsync_vnode->Close(), ZX_OK);
  invalid_fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 4. SPO
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 5. Remount with roll-forward recovery
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  curr_checkpoint_ver = fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver;
  ASSERT_EQ(pre_checkpoint_ver + 1, curr_checkpoint_ver);

  // 6. Check fsynced file
  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // Valid File can be successfully recovered
  fbl::RefPtr<fs::Vnode> file_fs_vnode;
  File *fsync_file_ptr;
  FileTester::Lookup(root_dir.get(), valid_file_name, &file_fs_vnode);
  valid_fsync_vnode = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(file_fs_vnode));
  fsync_file_ptr = static_cast<File *>(valid_fsync_vnode.get());
  ASSERT_EQ(valid_fsync_vnode->GetSize(), data_page_count * PAGE_SIZE);

  for (uint32_t index = 0; index < data_page_count; ++index) {
    uint32_t write_buf[PAGE_SIZE / (sizeof(uint32_t) / sizeof(uint8_t))];
    FileTester::ReadFromFile(fsync_file_ptr, write_buf, PAGE_SIZE,
                             static_cast<size_t>(index) * PAGE_SIZE);
    ASSERT_EQ(write_buf[0], index);
  }

  // Currupted invalid file cannot be recovered
  FileTester::Lookup(root_dir.get(), invalid_file_name, &file_fs_vnode);
  ASSERT_EQ(file_fs_vnode, nullptr);

  ASSERT_EQ(valid_fsync_vnode->Close(), ZX_OK);
  valid_fsync_vnode = nullptr;
  invalid_fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 7. Unmount and check filesystem
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(FsyncRecoveryTest, Fdatasync) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable roll-forward recovery
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // 1. Create file
  const pgoff_t data_page_count = kAddrsPerInode + 1;
  auto ret = CreateFileAndWritePages(root_dir.get(), "fsync_dnode_file", data_page_count, 0);
  ASSERT_TRUE(ret.is_ok());
  auto fsync_vnode = std::move(ret.value());
  ino_t fsync_file_ino = fsync_vnode->Ino();

  File *file = static_cast<File *>(fsync_vnode.get());
  size_t out;
  char r_buf[kPageSize];
  ASSERT_EQ(FileTester::Read(file, r_buf, kPageSize, kAddrsPerInode * kPageSize, &out), ZX_OK);

  char w_buf[kPageSize];
  std::memset(w_buf, 0xFF, kPageSize);
  ASSERT_EQ(FileTester::Write(file, w_buf, kPageSize, kAddrsPerInode * kPageSize, &out), ZX_OK);

  // 2. Checkpoint
  fs->SyncFs(true);

  // 3. Write the last block that causes updates on dnode
  ASSERT_EQ(FileTester::Write(file, r_buf, kPageSize, kAddrsPerInode * kPageSize, &out), ZX_OK);

  // 4. Request fdatasync() to log the dnode
  ASSERT_EQ(fsync_vnode->SyncFile(true), ZX_OK);
  ASSERT_EQ(fsync_vnode->Close(), ZX_OK);
  fsync_vnode = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  // 5. SPO and check blocks to be recovered
  FileTester::SuddenPowerOff(std::move(fs), &bc);
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 1), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  CheckFsyncedFile(fs.get(), fsync_file_ino, 1, 1);

  // 6. SPO and check the recovery
  FileTester::SuddenPowerOff(std::move(fs), &bc);
  ASSERT_EQ(options.SetValue(MountOption::kDisableRollForward, 0), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

}  // namespace
}  // namespace f2fs
