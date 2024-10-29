// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zircon-internal/thread_annotations.h>

#include <algorithm>
#include <random>

#include <gtest/gtest.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

constexpr uint64_t kDefaultBlockCount = 143360;
class GcTest : public F2fsFakeDevTestFixture {
 public:
  GcTest(TestOptions options = TestOptions{.block_count = kDefaultBlockCount})
      : F2fsFakeDevTestFixture(std::move(options)) {
    // GcTest should run with MountOption::kForceLfs set.
    mount_options_.SetValue(MountOption::kForceLfs, true);
  }

 protected:
  std::vector<std::string> MakeGcTriggerCondition(uint32_t invalidate_ratio = 25,
                                                  bool sync = true) {
    auto prng = std::default_random_engine(testing::UnitTest::GetInstance()->random_seed());

    fs_->GetSegmentManager().DisableFgGc();
    std::vector<std::string> total_file_names;
    uint32_t count = 0;
    while (true) {
      if (fs_->GetSegmentManager().HasNotEnoughFreeSecs()) {
        break;
      }
      std::vector<std::string> file_names;
      for (uint32_t i = 0; i < fs_->GetSuperblockInfo().GetBlocksPerSeg() &&
                           !fs_->GetSegmentManager().HasNotEnoughFreeSecs();
           ++i, ++count) {
        std::string file_name = std::to_string(count);
        zx::result test_file = root_dir_->Create(file_name, fs::CreationType::kFile);
        EXPECT_TRUE(test_file.is_ok()) << test_file.status_string();
        auto file_vn = fbl::RefPtr<File>::Downcast(*std::move(test_file));
        std::array<char, kPageSize> buf;
        f2fs_hash_t hash = DentryHash(file_name);
        std::memcpy(buf.data(), &hash, sizeof(hash));
        FileTester::AppendToFile(file_vn.get(), buf.data(), buf.size());
        file_names.push_back(file_name);
        EXPECT_EQ(file_vn->Close(), ZX_OK);
        file_vn->Writeback(true, true);
      }
      sync_completion_t completion;
      fs_->GetWriter().ScheduleWriteBlocks(&completion);
      sync_completion_wait(&completion, ZX_TIME_INFINITE);
      if (sync) {
        fs_->SyncFs();
      }

      std::shuffle(file_names.begin(), file_names.end(), prng);

      const uint64_t kDeleteSize = file_names.size() * invalidate_ratio / 100;
      auto iter = file_names.begin();
      for (uint64_t i = 0; i < kDeleteSize; ++i) {
        EXPECT_EQ(root_dir_->Unlink(*iter, false), ZX_OK);
        iter = file_names.erase(iter);
      }
      if (sync) {
        fs_->SyncFs();
      }
      total_file_names.insert(total_file_names.end(), file_names.begin(), file_names.end());
    }

    fs_->GetSegmentManager().EnableFgGc();
    return total_file_names;
  }
};

TEST_F(GcTest, CpError) TA_NO_THREAD_SAFETY_ANALYSIS {
  fs_->GetSuperblockInfo().SetCpFlags(CpFlag::kCpErrorFlag);
  auto result = fs_->StartGc();
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(result.error_value(), ZX_ERR_BAD_STATE);
}

TEST_F(GcTest, CheckpointDiskReadFailOnSyncFs) TA_NO_THREAD_SAFETY_ANALYSIS {
  DisableFsck();

  fs_->GetMetaVnode().Writeback(true, true);

  pgoff_t target_addr = fs_->GetSegmentManager().CurrentSitAddr(0) * kDefaultSectorsPerBlock;

  auto hook = [target_addr](const block_fifo_request_t &_req, const zx::vmo *_vmo) {
    if (_req.dev_offset == target_addr) {
      return ZX_ERR_PEER_CLOSED;
    }
    return ZX_OK;
  };

  MakeGcTriggerCondition();

  // Increse dirty page count to perform GC
  {
    fs_->GetSuperblockInfo().IncreasePageCount(CountType::kDirtyData);

    DeviceTester::SetHook(fs_.get(), hook);
    ASSERT_EQ(fs_->SyncFs(true), ZX_ERR_INTERNAL);
    ASSERT_TRUE(fs_->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag));

    DeviceTester::SetHook(fs_.get(), nullptr);

    fs_->GetSuperblockInfo().DecreasePageCount(CountType::kDirtyData);
  }
}

TEST_F(GcTest, CheckpointDiskReadFailOnGc) TA_NO_THREAD_SAFETY_ANALYSIS {
  DisableFsck();

  fs_->GetMetaVnode().Writeback(true, true);

  pgoff_t target_addr = fs_->GetSegmentManager().CurrentSitAddr(0) * kDefaultSectorsPerBlock;

  auto hook = [target_addr](const block_fifo_request_t &_req, const zx::vmo *_vmo) {
    if (_req.dev_offset == target_addr) {
      return ZX_ERR_PEER_CLOSED;
    }
    return ZX_OK;
  };

  MakeGcTriggerCondition();

  // Check disk peer closed exception case in Run()
  {
    DeviceTester::SetHook(fs_.get(), hook);
    ASSERT_EQ(fs_->StartGc().error_value(), ZX_ERR_PEER_CLOSED);
    ASSERT_TRUE(fs_->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag));

    DeviceTester::SetHook(fs_.get(), nullptr);
  }
}

TEST_F(GcTest, CheckpointDiskReadFailOnGcPreFree) TA_NO_THREAD_SAFETY_ANALYSIS {
  DisableFsck();

  fs_->GetMetaVnode().Writeback(true, true);

  pgoff_t target_addr = fs_->GetSegmentManager().CurrentSitAddr(0) * kDefaultSectorsPerBlock;

  auto hook = [target_addr](const block_fifo_request_t &_req, const zx::vmo *_vmo) {
    if (_req.dev_offset == target_addr) {
      return ZX_ERR_PEER_CLOSED;
    }
    return ZX_OK;
  };

  uint32_t prefree_segno = fs_->GetSegmentManager().CURSEG_I(CursegType::kCursegWarmData)->segno;
  MakeGcTriggerCondition(25, false);

  fs_->GetSegmentManager().LocateDirtySegment(prefree_segno + 1, DirtyType::kPre);

  // Check disk peer closed exception case in Run()
  {
    DeviceTester::SetHook(fs_.get(), hook);
    ASSERT_EQ(fs_->StartGc().error_value(), ZX_ERR_PEER_CLOSED);

    DeviceTester::SetHook(fs_.get(), nullptr);
  }
}

TEST_F(GcTest, PageColdData) {
  fs_->GetSegmentManager().DisableFgGc();
  zx::result test_file = root_dir_->Create("file", fs::CreationType::kFile);
  ASSERT_TRUE(test_file.is_ok()) << test_file.status_string();
  auto file = fbl::RefPtr<File>::Downcast(*std::move(test_file));

  char buf[kPageSize] = {
      0,
  };
  FileTester::AppendToFile(file.get(), buf, sizeof(buf));
  file->Writeback(true, true);

  MakeGcTriggerCondition(10);
  fs_->GetSegmentManager().DisableFgGc();

  // Get old block address.
  zx::result old_addrs_or = file->GetDataBlockAddresses(0, 1, true);
  ASSERT_TRUE(old_addrs_or.is_ok());

  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    auto pages_or = file->WriteBegin(0, kPageSize);
    ASSERT_TRUE(pages_or.is_ok());
    ASSERT_TRUE(pages_or->front()->IsDirty());
  }
  // If kPageColdData flag is not set, allocate its block as SSR or LFS.
  ASSERT_NE(file->Writeback(true, true), 0UL);
  zx::result new_addrs_or = file->GetDataBlockAddresses(0, 1, true);
  ASSERT_TRUE(new_addrs_or.is_ok());
  ASSERT_NE(new_addrs_or->front(), old_addrs_or->front());

  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    auto pages_or = file->WriteBegin(0, kPageSize);
    ASSERT_TRUE(pages_or.is_ok());
    ASSERT_TRUE(pages_or->front()->IsDirty());
    pages_or->front()->SetColdData();
  }
  // If kPageColdData flag is set, allocate its block as LFS.
  CursegInfo *cold_curseg = fs_->GetSegmentManager().CURSEG_I(CursegType::kCursegColdData);
  auto expected_addr = fs_->GetSegmentManager().NextFreeBlkAddr(CursegType::kCursegColdData);
  ASSERT_EQ(cold_curseg->alloc_type, static_cast<uint8_t>(AllocMode::kLFS));
  ASSERT_NE(file->Writeback(true, true), 0UL);
  new_addrs_or = file->GetDataBlockAddresses(0, 1, true);
  ASSERT_TRUE(old_addrs_or.is_ok());
  ASSERT_NE(new_addrs_or->front(), old_addrs_or->front());
  ASSERT_EQ(new_addrs_or->front(), expected_addr);
  {
    LockedPage data_page;
    ASSERT_EQ(file->GrabLockedPage(0, &data_page), ZX_OK);
    ASSERT_FALSE(data_page->IsColdData());
  }

  file->Close();
}

TEST_F(GcTest, OrphanFileGc) {
  DirtySeglistInfo *dirty_info = &fs_->GetSegmentManager().GetDirtySegmentInfo();
  FreeSegmapInfo *free_info = &fs_->GetSegmentManager().GetFreeSegmentInfo();

  zx::result vn = root_dir_->Create("test", fs::CreationType::kFile);
  ASSERT_TRUE(vn.is_ok()) << vn.status_string();
  auto file = fbl::RefPtr<File>::Downcast(*std::move(vn));

  uint8_t buffer[kPageSize] = {
      0xAA,
  };
  FileTester::AppendToFile(file.get(), buffer, kPageSize);
  file->Writeback(true, true);

  fs_->GetSegmentManager().AllocateNewSegments();
  fs_->SyncFs();

  zx::result addrs_or = file->GetDataBlockAddresses(0, 1, true);
  ASSERT_TRUE(addrs_or.is_ok());

  // gc target segno
  auto target_segno = fs_->GetSegmentManager().GetSegmentNumber(addrs_or->front());

  // Check victim seg is dirty
  ASSERT_TRUE(dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(target_segno));
  ASSERT_TRUE(free_info->free_segmap.GetOne(target_segno));

  // Make file orphan
  FileTester::DeleteChild(root_dir_.get(), "test", false);
  ASSERT_NE(file->GetBlocks(), 0U);

  // Do gc
  ASSERT_EQ(GcTester::DoGarbageCollect(fs_->GetSegmentManager(), target_segno, GcType::kFgGc),
            ZX_OK);

  // Check if gc purges the metadata when the victim blocks belong to an orphan
  ASSERT_EQ(file->GetBlocks(), 0U);

  // Check victim seg is clean
  ASSERT_FALSE(dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(target_segno));

  // Check if the paged vmo keeps data
  uint8_t read[kPageSize] = {
      0,
  };
  FileTester::ReadFromFile(file.get(), read, sizeof(read), 0);
  ASSERT_EQ(std::memcmp(buffer, read, sizeof(read)), 0);

  ASSERT_EQ(file->Close(), ZX_OK);
  file = nullptr;
}

TEST_F(GcTest, ReadVmoDuringGc) {
  DirtySeglistInfo *dirty_info = &fs_->GetSegmentManager().GetDirtySegmentInfo();
  FreeSegmapInfo *free_info = &fs_->GetSegmentManager().GetFreeSegmentInfo();

  zx::result vn = root_dir_->Create("test", fs::CreationType::kFile);
  ASSERT_TRUE(vn.is_ok()) << vn.status_string();
  auto file = fbl::RefPtr<File>::Downcast(*std::move(vn));

  uint8_t buffer[kPageSize] = {
      0xAA,
  };
  FileTester::AppendToFile(file.get(), buffer, kPageSize);
  file->Writeback(true, true);

  fs_->GetSegmentManager().AllocateNewSegments();
  fs_->SyncFs();

  zx::result addrs_or = file->GetDataBlockAddresses(0, 1, true);
  ASSERT_TRUE(addrs_or.is_ok());

  // gc target segno
  auto target_segno = fs_->GetSegmentManager().GetSegmentNumber(addrs_or->front());

  // Check victim seg is dirty
  ASSERT_TRUE(dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(target_segno));
  ASSERT_TRUE(free_info->free_segmap.GetOne(target_segno));

  // Evict |file| to delete paged_vmo
  ASSERT_EQ(file->Close(), ZX_OK);
  ASSERT_EQ(fs_->GetVCache().Evict(file.get()), ZX_OK);
  file.reset();

  // Do gc
  ASSERT_EQ(GcTester::DoGarbageCollect(fs_->GetSegmentManager(), target_segno, GcType::kFgGc),
            ZX_OK);

  // Check victim seg is clean
  ASSERT_FALSE(dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(target_segno));

  fbl::RefPtr<fs::Vnode> vnode;
  FileTester::Lookup(root_dir_.get(), "test", &vnode);
  file = fbl::RefPtr<File>::Downcast(std::move(vnode));
  // Check data after gc
  uint8_t read[kPageSize] = {
      0,
  };
  FileTester::ReadFromFile(file.get(), read, sizeof(read), 0);
  ASSERT_EQ(std::memcmp(buffer, read, sizeof(read)), 0);

  ASSERT_EQ(file->Close(), ZX_OK);
  file = nullptr;
}

class GcTestWithLargeSec : public GcTest,
                           public testing::WithParamInterface<std::pair<uint64_t, uint32_t>> {
 public:
  GcTestWithLargeSec()
      : GcTest(TestOptions{.block_count = GetParam().first,
                           .mkfs_options = MkfsOptions{.segs_per_sec = GetParam().second}}) {}
};

TEST_P(GcTestWithLargeSec, SegmentDirtyInfo) TA_NO_THREAD_SAFETY_ANALYSIS {
  MakeGcTriggerCondition();
  DirtySeglistInfo *dirty_info = &fs_->GetSegmentManager().GetDirtySegmentInfo();

  // Get Victim
  uint32_t last_victim =
      fs_->GetSegmentManager().GetLastVictim(static_cast<int>(GcMode::kGcGreedy));
  auto victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kFgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_FALSE(victim_seg_or.is_error());
  uint32_t victim_seg = victim_seg_or.value();
  fs_->GetSegmentManager().SetLastVictim(static_cast<int>(GcMode::kGcGreedy), last_victim);
  fs_->GetSegmentManager().SetCurVictimSec(kNullSecNo);

  // Check at least one of victim seg is dirty
  bool is_dirty = false;
  const uint32_t start_segno = victim_seg - (victim_seg % fs_->GetSuperblockInfo().GetSegsPerSec());
  for (uint32_t i = 0; i < fs_->GetSuperblockInfo().GetSegsPerSec(); ++i) {
    is_dirty |=
        dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(start_segno + i);
  }
  ASSERT_TRUE(is_dirty);

  // Copy prev nr_dirty info
  int prev_nr_dirty[static_cast<int>(DirtyType::kNrDirtytype)] = {};
  memcpy(prev_nr_dirty, dirty_info->nr_dirty, sizeof(prev_nr_dirty));

  // Trigger GC
  auto result = fs_->StartGc();
  ASSERT_FALSE(result.is_error());

  // Check victim seg is clean
  for (uint32_t i = 0; i < fs_->GetSuperblockInfo().GetSegsPerSec(); ++i) {
    ASSERT_FALSE(
        dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(start_segno + i));
  }

  // Check nr_dirty decreased
  for (int i = static_cast<int>(DirtyType::kDirtyHotData); i <= static_cast<int>(DirtyType::kDirty);
       ++i) {
    ASSERT_TRUE(dirty_info->nr_dirty[i] <= prev_nr_dirty[i]);
  }
}

TEST_P(GcTestWithLargeSec, SegmentFreeInfo) TA_NO_THREAD_SAFETY_ANALYSIS {
  MakeGcTriggerCondition();
  FreeSegmapInfo *free_info = &fs_->GetSegmentManager().GetFreeSegmentInfo();

  // Get Victim
  uint32_t last_victim =
      fs_->GetSegmentManager().GetLastVictim(static_cast<int>(GcMode::kGcGreedy));
  auto victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kFgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_FALSE(victim_seg_or.is_error());
  uint32_t victim_seg = victim_seg_or.value();
  fs_->GetSegmentManager().SetLastVictim(static_cast<int>(GcMode::kGcGreedy), last_victim);
  fs_->GetSegmentManager().SetCurVictimSec(kNullSecNo);
  uint32_t victim_sec = fs_->GetSegmentManager().GetSecNo(victim_seg);

  // Check victim sec is not free
  ASSERT_TRUE(free_info->free_secmap.GetOne(victim_sec));

  // Trigger GC
  auto result = fs_->StartGc();
  ASSERT_FALSE(result.is_error());

  // Check victim sec is freed
  ASSERT_FALSE(free_info->free_secmap.GetOne(victim_sec));
}

TEST_P(GcTestWithLargeSec, SecureSpace) {
  MakeGcTriggerCondition();
  // Set the number of blocks to be gc'ed as 2 sections or available space of the volume.
  uint64_t blocks_to_secure =
      std::min((safemath::CheckMul<uint64_t>(fs_->GetSuperblockInfo().GetTotalBlockCount(),
                                             100 - fs_->GetSegmentManager().Utilization()) /
                100)
                   .ValueOrDie(),
               (safemath::CheckMul<uint64_t>(2, fs_->GetSuperblockInfo().GetBlocksPerSeg()) *
                fs_->GetSuperblockInfo().GetSegsPerSec())
                   .ValueOrDie());

  // It should be able to create new files on free blocks that gc acquires.
  for (uint32_t i = 0; i < blocks_to_secure; ++i) {
    std::string file_name = "_" + std::to_string(i);
    zx::result test_file = root_dir_->Create(file_name, fs::CreationType::kFile);
    ASSERT_TRUE(test_file.is_ok()) << test_file.status_string();
    test_file->Close();
  }
}

TEST_P(GcTestWithLargeSec, GcConsistency) TA_NO_THREAD_SAFETY_ANALYSIS {
  std::vector<std::string> file_names = MakeGcTriggerCondition();

  // It secures enough free sections.
  auto secs_or = fs_->StartGc();
  ASSERT_TRUE(secs_or.is_ok());

  for (auto name : file_names) {
    fbl::RefPtr<fs::Vnode> vn;
    FileTester::Lookup(root_dir_.get(), name, &vn);
    ASSERT_TRUE(vn);
    auto file = fbl::RefPtr<File>::Downcast(std::move(vn));
    char buf[kPageSize] = {
        0,
    };
    FileTester::ReadFromFile(file.get(), buf, sizeof(buf), 0);
    f2fs_hash_t hash = DentryHash(name);
    ASSERT_EQ(std::memcmp(buf, &hash, sizeof(hash)), 0);
    file->Close();
  }
}

const std::array<std::pair<uint64_t, uint32_t>, 2> kSecParams = {
    {{kDefaultBlockCount, 1}, {4 * kDefaultBlockCount, 4}}};
INSTANTIATE_TEST_SUITE_P(GcTestWithLargeSec, GcTestWithLargeSec, ::testing::ValuesIn(kSecParams));

}  // namespace
}  // namespace f2fs
