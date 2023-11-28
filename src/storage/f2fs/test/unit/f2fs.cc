// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {
using Runner = ComponentRunner;

static void WriteSuperblock(const Superblock &sb, BcacheMapper &bc) {
  BlockBuffer block;
  memcpy(block.get<uint8_t>() + kSuperOffset, &sb, sizeof(Superblock));
  bc.Writeblk(0, &block);
  bc.Writeblk(1, &block);
}

TEST(SuperblockTest, SanityCheckRawSuper) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});
  auto superblock = LoadSuperblock(*bc);
  ASSERT_TRUE(superblock.is_ok());

  // Check exception cases
  BlockBuffer<Superblock> corrupted;
  std::memcpy(&corrupted, (*superblock).get(), sizeof(Superblock));
  corrupted->log_sectors_per_block = kDefaultSectorsPerBlock;
  corrupted->log_sectorsize = kMaxLogSectorSize;
  WriteSuperblock(*corrupted, *bc);
  ASSERT_EQ(LoadSuperblock(*bc).status_value(), ZX_ERR_INVALID_ARGS);

  std::memcpy(&corrupted, (*superblock).get(), sizeof(Superblock));
  corrupted->log_sectorsize = kMaxLogSectorSize + 1;
  WriteSuperblock(*corrupted, *bc);
  ASSERT_EQ(LoadSuperblock(*bc).status_value(), ZX_ERR_INVALID_ARGS);

  std::memcpy(&corrupted, (*superblock).get(), sizeof(Superblock));
  corrupted->log_blocksize = kMaxLogSectorSize + 1;
  WriteSuperblock(*corrupted, *bc);
  ASSERT_EQ(LoadSuperblock(*bc).status_value(), ZX_ERR_INVALID_ARGS);

  std::memcpy(&corrupted, (*superblock).get(), sizeof(Superblock));
  corrupted->magic = 0xF2F5FFFF;
  corrupted->log_blocksize = kMaxLogSectorSize + 1;
  WriteSuperblock(*corrupted, *bc);
  ASSERT_EQ(LoadSuperblock(*bc).status_value(), ZX_ERR_INVALID_ARGS);
}

TEST(SuperblockTest, GetValidCheckpoint) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  // Create a vfs object for unit tests.
  auto vfs_or = Runner::CreateRunner(loop.dispatcher());
  ZX_ASSERT(vfs_or.is_ok());
  std::unique_ptr<F2fs> fs =
      std::make_unique<F2fs>(loop.dispatcher(), std::move(bc), MountOptions{}, (*vfs_or).get());

  auto superblock = LoadSuperblock(fs->GetBc());
  ASSERT_TRUE(superblock.is_ok());
  superblock->cp_blkaddr = LeToCpu(superblock->cp_blkaddr) + 2;
  // Check GetValidCheckpoint exception case
  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_ERR_INVALID_ARGS);

  fs->GetVCache().Reset();
  fs->Reset();
}

TEST(SuperblockTest, SanityCheckCkpt) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  // Create a vfs object for unit tests.
  auto vfs_or = Runner::CreateRunner(loop.dispatcher());
  ZX_ASSERT(vfs_or.is_ok());
  std::unique_ptr<F2fs> fs =
      std::make_unique<F2fs>(loop.dispatcher(), std::move(bc), MountOptions{}, (*vfs_or).get());

  // Check SanityCheckCkpt exception case
  auto superblock = LoadSuperblock(fs->GetBc());
  ASSERT_TRUE(superblock.is_ok());
  superblock->segment_count_nat = 0;
  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_ERR_BAD_STATE);

  superblock = LoadSuperblock(fs->GetBc());
  ASSERT_TRUE(superblock.is_ok());
  superblock->segment_count = 0;
  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_ERR_BAD_STATE);

  fs->GetVCache().Reset();
  fs->Reset();
}

TEST(SuperblockTest, Reset) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  // Create a vfs object for unit tests.
  auto vfs_or = Runner::CreateRunner(loop.dispatcher());
  ZX_ASSERT(vfs_or.is_ok());
  std::unique_ptr<F2fs> fs =
      std::make_unique<F2fs>(loop.dispatcher(), std::move(bc), MountOptions{}, (*vfs_or).get());

  auto superblock = LoadSuperblock(fs->GetBc());
  ASSERT_TRUE(superblock.is_ok());
  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_OK);
  fs->GetVCache().Reset();

  ASSERT_TRUE(fs->IsValid());
  fs->ResetGcManager();
  ASSERT_FALSE(fs->IsValid());
  fs->ResetNodeManager();
  ASSERT_FALSE(fs->IsValid());
  fs->ResetSegmentManager();
  ASSERT_FALSE(fs->IsValid());
  fs->ResetPsuedoVnodes();
  ASSERT_FALSE(fs->IsValid());
  fs->ResetSuperblockInfo();
  ASSERT_FALSE(fs->IsValid());
  ASSERT_TRUE(fs->GetRootVnode().is_error());

  superblock = LoadSuperblock(fs->GetBc());
  ASSERT_TRUE(superblock.is_ok());
  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_OK);
  fs->GetVCache().Reset();

  ASSERT_TRUE(fs->IsValid());
  fs->Reset();
  ASSERT_FALSE(fs->IsValid());
}

TEST(F2fsTest, TakeBc) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});
  BcacheMapper *bcache_ptr = bc.get();

  std::unique_ptr<F2fs> fs;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), MountOptions{}, &bc, &fs);
  ASSERT_TRUE(fs->IsValid());
  ASSERT_EQ(&fs->GetBc(), bcache_ptr);

  fs->Sync();
  fs->PutSuper();
  auto bc_or = fs->TakeBc();
  ASSERT_TRUE(bc_or.is_ok());
  ASSERT_TRUE(fs->TakeBc().is_error());
  ASSERT_FALSE(fs->IsValid());
  fs.reset();
  bc = std::move(*bc_or);
  ASSERT_EQ(bc.get(), bcache_ptr);

  FileTester::MkfsOnFakeDevWithOptions(&bc, {});
  FileTester::MountWithOptions(loop.dispatcher(), MountOptions{}, &bc, &fs);

  fs->Sync();
  fs->PutSuper();
  ASSERT_TRUE(fs->TakeBc().is_ok());
  ASSERT_TRUE(fs->TakeBc().is_error());
  ASSERT_FALSE(fs->IsValid());
}

TEST(F2fsTest, BlockBuffer) {
  BlockBuffer block;
  uint8_t data[kBlockSize];
  memset(data, 0xaa, kBlockSize);
  memcpy(&block, data, kBlockSize);
  ASSERT_EQ(memcmp(&block, data, kBlockSize), 0);

  BlockBuffer block2(block);
  ASSERT_EQ(memcmp(&block2, data, kBlockSize), 0);
  memset(&block2, 0xbb, kBlockSize);

  block = block2;
  ASSERT_EQ(memcmp(&block, &block2, kBlockSize), 0);
}

TEST(F2fsTest, GetFilesystemInfo) {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});

  std::unique_ptr<F2fs> fs;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), MountOptions{}, &bc, &fs);

  auto &sb_info = fs->GetSuperblockInfo();
  auto info_or = fs->GetFilesystemInfo();
  ASSERT_TRUE(info_or.is_ok());
  auto info = info_or.value();

  ASSERT_EQ(info.block_size, kBlockSize);
  ASSERT_EQ(info.max_filename_size, kMaxNameLen);
  ASSERT_EQ(info.fs_type, fuchsia_fs::VfsType::kF2Fs);
  ASSERT_EQ(info.total_bytes, sb_info.GetTotalBlockCount() * kBlockSize);
  ASSERT_EQ(info.used_bytes, sb_info.GetValidBlockCount() * kBlockSize);
  ASSERT_EQ(info.total_nodes, sb_info.GetMaxNodeCount());
  ASSERT_EQ(info.used_nodes, sb_info.GetValidInodeCount());
  ASSERT_EQ(info.name, "f2fs");

  // Check type conversion
  block_t tmp_user_block_count = sb_info.GetTotalBlockCount();
  block_t tmp_valid_block_count = sb_info.GetTotalBlockCount();

  constexpr uint64_t LARGE_BLOCK_COUNT = 26214400;  // 100GB

  sb_info.SetTotalBlockCount(LARGE_BLOCK_COUNT);
  sb_info.SetValidBlockCount(LARGE_BLOCK_COUNT);

  info_or = fs->GetFilesystemInfo();
  ASSERT_TRUE(info_or.is_ok());
  info = info_or.value();

  ASSERT_EQ(info.total_bytes, LARGE_BLOCK_COUNT * kBlockSize);
  ASSERT_EQ(info.used_bytes, LARGE_BLOCK_COUNT * kBlockSize);

  sb_info.SetTotalBlockCount(tmp_user_block_count);
  sb_info.SetValidBlockCount(tmp_valid_block_count);
  FileTester::Unmount(std::move(fs), &bc);
}

}  // namespace
}  // namespace f2fs
