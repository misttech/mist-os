// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_SEGMENT_H_
#define SRC_STORAGE_F2FS_SEGMENT_H_

#include "src/storage/f2fs/bitmap.h"
#include "src/storage/f2fs/common.h"
#include "src/storage/f2fs/layout.h"
#include "src/storage/f2fs/superblock_info.h"

namespace f2fs {

class F2fs;
class LockedPage;
class SuperblockInfo;

// constant macro
constexpr uint32_t kNullSegNo = std::numeric_limits<uint32_t>::max();
constexpr uint32_t kNullSecNo = std::numeric_limits<uint32_t>::max();
constexpr uint32_t kUint32Max = std::numeric_limits<uint32_t>::max();
constexpr uint32_t kMaxSearchLimit = 4096;

// Indicate a block allocation direction:
// kAllocRight means allocating new sections towards the end of volume.
// kAllocLeft means the opposite direction.
enum class AllocDirection {
  kAllocRight = 0,
  kAllocLeft,
};

// In the VictimSelPolicy->alloc_mode, there are two block allocation modes.
// LFS writes data sequentially with cleaning operations.
// SSR (Slack Space Recycle) reuses obsolete space without cleaning operations.
enum class AllocMode { kLFS = 0, kSSR };

// In the VictimSelPolicy->gc_mode, there are two gc, aka cleaning, modes.
// GC_CB is based on cost-benefit algorithm.
// GC_GREEDY is based on greedy algorithm.
enum class GcMode { kGcCb = 0, kGcGreedy };

// BG_GC means the background cleaning job.
// FG_GC means the on-demand cleaning job.
enum class GcType { kBgGc = 0, kFgGc };

// for a function parameter to select a victim segment
struct VictimSelPolicy {
  AllocMode alloc_mode = AllocMode::kLFS;  // LFS or SSR
  GcMode gc_mode = GcMode::kGcCb;          // cost effective or greedy
  RawBitmap *dirty_segmap = nullptr;       // dirty segment bitmap
  size_t max_search = kMaxSearchLimit;     // maximum # of segments to search
  size_t offset = 0;                       // last scanned bitmap offset
  size_t ofs_unit = 0;                     // bitmap search unit
  size_t min_cost = 0;                     // minimum cost
  uint32_t min_segno = 0;                  // segment # having min. cost
};

// SSR mode uses these field to determine which blocks are allocatable.
struct SegmentEntry {
  RawBitmapHeap cur_valid_map;     // validity bitmap of blocks
  RawBitmapHeap ckpt_valid_map;    // validity bitmap in the last CP
  uint64_t mtime = 0;              // modification time of the segment
  uint16_t valid_blocks = 0;       // # of valid blocks
  uint16_t ckpt_valid_blocks = 0;  // # of valid blocks in the last CP
  uint8_t type = 0;                // segment type like CursegType::XXX
};

struct SectionEntry {
  uint32_t valid_blocks = 0;  // # of valid blocks in a section
};

struct SitInfo {
  RawBitmap sit_bitmap;                         // SIT bitmap pointer
  RawBitmap dirty_sentries_bitmap;              // bitmap for dirty sentries
  std::unique_ptr<SegmentEntry[]> sentries;     // SIT segment-level cache
  std::unique_ptr<SectionEntry[]> sec_entries;  // SIT section-level cache
  block_t sit_base_addr = 0;                    // start block address of SIT area
  block_t sit_blocks = 0;                       // # of blocks used by SIT area
  block_t written_valid_blocks = 0;             // # of valid blocks in main area
  uint32_t bitmap_size = 0;                     // SIT bitmap size
  uint32_t dirty_sentries = 0;                  // # of dirty sentries
  // for cost-benefit algorithm in cleaning procedure
  uint64_t elapsed_time = 0;  // elapsed time after mount
  uint64_t mounted_time = 0;  // mount time
  uint64_t min_mtime = 0;     // min. modification time
  uint64_t max_mtime = 0;     // max. modification time
};

struct FreeSegmapInfo {
  RawBitmap free_segmap;       // free segment bitmap
  RawBitmap free_secmap;       // free section bitmap
  uint32_t free_segments = 0;  // # of free segments
  uint32_t free_sections = 0;  // # of free sections
};

// Notice: The order of dirty type is same with CursegType::XXX
enum class DirtyType {
  kDirtyHotData = 0,  // dirty segments assigned as hot data logs
  kDirtyWarmData,     // dirty segments assigned as warm data logs
  kDirtyColdData,     // dirty segments assigned as cold data logs
  kDirtyHotNode,      // dirty segments assigned as hot node logs
  kDirtyWarmNode,     // dirty segments assigned as warm node logs
  kDirtyColdNode,     // dirty segments assigned as cold node logs
  kDirty,             // to count # of dirty segments
  kPre,               // to count # of entirely obsolete segments
  kNrDirtytype
};

struct DirtySeglistInfo {
  RawBitmap dirty_segmap[static_cast<int>(DirtyType::kNrDirtytype)];
  int nr_dirty[static_cast<int>(DirtyType::kNrDirtytype)] = {};  // # of dirty segments
  RawBitmap victim_secmap;                                       // background gc victims
};

// for active log information
struct CursegInfo {
  BlockBuffer<SummaryBlock> sum_blk;
  uint32_t segno = 0;              // current segment number
  uint32_t zone = 0;               // current zone number
  uint32_t next_segno = 0;         // preallocated segment
  std::shared_mutex curseg_mutex;  // lock for consistency
  uint16_t next_blkoff = 0;        // next block offset to write
  uint8_t alloc_type = 0;          // current allocation type
};

// For SIT manager
//
// By default, there are 6 active log areas across the whole main area.
// When considering hot and cold data separation to reduce cleaning overhead,
// we split 3 for data logs and 3 for node logs as hot, warm, and cold types,
// respectively.
// In the current design, you should not change the numbers intentionally.
// Instead, as a mount option such as active_logs=x, you can use 2, 4, and 6
// logs individually according to the underlying devices. (default: 6)
// Just in case, on-disk layout covers maximum 16 logs that consist of 8 for
// data and 8 for node logs.
constexpr int kNrCursegDataType = 3;
constexpr int kNrCursegNodeType = 3;
constexpr int kNrCursegType = kNrCursegDataType + kNrCursegNodeType;

enum class CursegType {
  kCursegHotData = 0,  // directory entry blocks
  kCursegWarmData,     // data blocks
  kCursegColdData,     // multimedia or GCed data blocks
  kCursegHotNode,      // direct node blocks of directory files
  kCursegWarmNode,     // direct node blocks of normal files
  kCursegColdNode,     // indirect node blocks
  kNoCheckType
};

using SummaryCallback = fit::function<zx_status_t(SummaryBlock &sum)>;

CursegType GetSegmentType(Page &page, PageType p_type, size_t num_logs);
int LookupJournalInCursum(SummaryBlock &sum, JournalType type, uint32_t val, int alloc);
int UpdateNatsInCursum(SummaryBlock &raw_summary, int i);
inline block_t SitBlockOffset(uint32_t segno) { return segno / kSitEntryPerBlock; }
inline block_t StartSegNo(uint32_t segno) { return SitBlockOffset(segno) * kSitEntryPerBlock; }
inline bool IsDataSeg(const CursegType t) {
  return ((t == CursegType::kCursegHotData) || (t == CursegType::kCursegColdData) ||
          (t == CursegType::kCursegWarmData));
}
inline bool IsNodeSeg(const CursegType t) {
  return ((t == CursegType::kCursegHotNode) || (t == CursegType::kCursegColdNode) ||
          (t == CursegType::kCursegWarmNode));
}
inline void SetSummary(Summary *sum, nid_t nid, size_t ofs_in_node, uint8_t version) {
  sum->nid = CpuToLe(nid);
  sum->ofs_in_node = CpuToLe(safemath::checked_cast<uint16_t>(ofs_in_node));
  sum->version = version;
}

class SegmentManager {
 public:
  // Not copyable or moveable
  SegmentManager(const SegmentManager &) = delete;
  SegmentManager &operator=(const SegmentManager &) = delete;
  SegmentManager(SegmentManager &&) = delete;
  SegmentManager &operator=(SegmentManager &&) = delete;
  SegmentManager() = delete;
  explicit SegmentManager(F2fs *fs);
  explicit SegmentManager(SuperblockInfo &info);

  zx_status_t BuildFreeSegmap() __TA_EXCLUDES(segmap_lock_);
  zx_status_t BuildSegmentManager();
  void DestroySegmentManager();

  const SegmentEntry &GetSegmentEntry(uint32_t segno) __TA_EXCLUDES(sentry_lock_);
  void GetSitBitmap(void *dst_addr) __TA_EXCLUDES(sentry_lock_);

  bool CompareValidBlocks(uint32_t blocks, uint32_t segno, bool section)
      __TA_EXCLUDES(sentry_lock_);
  uint32_t GetValidBlocks(uint32_t segno, bool section) const __TA_REQUIRES_SHARED(sentry_lock_);
  bool HasNotEnoughFreeSecs(size_t freed = 0, size_t needed = 0);
  uint32_t Utilization();
  uint32_t CursegSegno(int type);
  uint8_t CursegAllocType(int type);
  uint16_t CursegBlkoff(int type);
  void CheckSegRange(uint32_t segno) const;
  void CheckBlockCount(uint32_t segno, SitEntry &raw_sit);
  pgoff_t CurrentSitAddr(uint32_t start) __TA_REQUIRES_SHARED(sentry_lock_);
  pgoff_t NextSitAddr(pgoff_t block_addr) __TA_REQUIRES_SHARED(sentry_lock_);
  void SetToNextSit(uint32_t start) __TA_REQUIRES(sentry_lock_);
  uint64_t GetMtime() const;
  block_t StartSumBlock() const;
  block_t SumBlkAddr(int base, int type) const;
  bool SecUsageCheck(uint32_t secno) const __TA_REQUIRES_SHARED(seglist_lock_);
  bool IsValidBlock(uint32_t segno, uint64_t offset) __TA_EXCLUDES(sentry_lock_);

  block_t PrefreeSegments() __TA_EXCLUDES(seglist_lock_);
  block_t FreeSections() __TA_EXCLUDES(segmap_lock_);
  block_t FreeSegments() __TA_EXCLUDES(segmap_lock_);
  block_t DirtySegments() __TA_EXCLUDES(seglist_lock_);
  block_t OverprovisionSegments();
  block_t OverprovisionSections();
  block_t ReservedSections();

  bool NeedSSR();
  bool NeedInplaceUpdate(bool is_dir);

  void LocateDirtySegment(uint32_t segno, enum DirtyType dirty_type) __TA_REQUIRES(seglist_lock_)
      __TA_REQUIRES_SHARED(sentry_lock_);
  void RemoveDirtySegment(uint32_t segno, enum DirtyType dirty_type) __TA_REQUIRES(seglist_lock_)
      __TA_REQUIRES_SHARED(sentry_lock_);
  void LocateDirtySegment(uint32_t segno) __TA_EXCLUDES(seglist_lock_)
      __TA_REQUIRES_SHARED(sentry_lock_);
  void SetPrefreeAsFreeSegments() __TA_EXCLUDES(seglist_lock_);
  void ClearPrefreeSegments() __TA_EXCLUDES(seglist_lock_);
  void MarkSitEntryDirty(uint32_t segno) __TA_REQUIRES(sentry_lock_);
  void SetSitEntryType(CursegType type, uint32_t segno, int modified) __TA_REQUIRES(sentry_lock_);
  void UpdateSitEntry(block_t blkaddr, int del) __TA_REQUIRES(sentry_lock_);
  void RefreshSitEntry(block_t old_blkaddr, block_t new_blkaddr) __TA_REQUIRES(sentry_lock_);
  void InvalidateBlocks(block_t addr) __TA_EXCLUDES(sentry_lock_);
  void AddSumEntry(CursegType type, Summary *sum, uint16_t offset);
  int NpagesForSummaryFlush();
  void GetSumPage(uint32_t segno, LockedPage *out);
  void WriteSumPage(SummaryBlock *sum_blk, block_t blk_addr);
  zx_status_t SetSummaryBlock(CursegType type, SummaryCallback callback);
  zx_status_t GetSummaryBlock(CursegType type, SummaryCallback callback);

  uint32_t CheckPrefreeSegments(int ofs_unit, CursegType type);
  void GetNewSegment(uint32_t *newseg, bool new_sec, AllocDirection dir);
  void ResetCurseg(CursegType type, int modified) __TA_REQUIRES(sentry_lock_);
  void NewCurseg(CursegType type, bool new_sec) __TA_REQUIRES(sentry_lock_);
  void NextFreeBlkoff(CursegInfo *seg, block_t start);
  void RefreshNextBlkoff(CursegInfo *seg);
  void ChangeCurseg(CursegType type, bool reuse) __TA_EXCLUDES(seglist_lock_)
      __TA_REQUIRES(sentry_lock_);
  void AllocateSegmentByDefault(CursegType type, bool force) __TA_EXCLUDES(seglist_lock_)
      __TA_REQUIRES(sentry_lock_);
  void AllocateNewSegments() __TA_EXCLUDES(sentry_lock_);
#if 0  // porting needed
  void VerifyBlockAddr(block_t blk_addr) = 0;
#endif
  bool HasCursegSpace(CursegType type);
  block_t GetBlockAddrOnSegment(LockedPage &page, block_t old_blkaddr, Summary *sum,
                                PageType p_type) __TA_EXCLUDES(sentry_lock_);
  void RecoverDataPage(Summary &sum, block_t old_blkaddr, block_t new_blkaddr);

  zx_status_t ReadCompactedSummaries() __TA_REQUIRES(sentry_lock_);
  zx_status_t ReadNormalSummaries(int type) __TA_REQUIRES(sentry_lock_);
  int RestoreCursegSummaries() __TA_EXCLUDES(sentry_lock_);
  zx_status_t RestoreNodeSummary(uint32_t segno, SummaryBlock &sum);

  void WriteCompactedSummaries(block_t blkaddr);
  void WriteNormalSummaries(block_t blkaddr, CursegType type);
  void WriteDataSummaries(block_t start_blk);
  void WriteNodeSummaries(block_t start_blk);

  bool FlushSitsInJournal() __TA_REQUIRES(sentry_lock_);
  zx_status_t FlushSitEntries() __TA_EXCLUDES(sentry_lock_);

  block_t GetMainAreaStartBlock() const { return main_blkaddr_; }
  CursegInfo *CURSEG_I(CursegType type) { return &curseg_array_[static_cast<int>(type)]; }
  const CursegInfo *CURSEG_I(CursegType type) const {
    return &curseg_array_[static_cast<int>(type)];
  }

  block_t StartBlock(uint32_t segno) const {
    return (seg0_blkaddr_ + (GetR2LSegNo(segno) << superblock_info_.GetLogBlocksPerSeg()));
  }
  block_t NextFreeBlkAddr(CursegType type) const {
    const CursegInfo *curseg = CURSEG_I(type);
    return (StartBlock(curseg->segno) + curseg->next_blkoff);
  }
  block_t GetSegOffFromSeg0(block_t blk_addr) const { return blk_addr - seg0_blkaddr_; }
  uint32_t GetSegNoFromSeg0(block_t blk_addr) const {
    return GetSegOffFromSeg0(blk_addr) >> superblock_info_.GetLogBlocksPerSeg();
  }
  uint32_t GetSegmentNumber(block_t blk_addr) {
    return ((blk_addr == kNullAddr) || (blk_addr == kNewAddr))
               ? kNullSegNo
               : GetL2RSegNo(GetSegNoFromSeg0(blk_addr));
  }
  uint32_t GetSecNo(uint32_t segno) const { return segno / superblock_info_.GetSegsPerSec(); }
  uint32_t GetZoneNoFromSegNo(uint32_t segno) const {
    return segno / superblock_info_.GetSegsPerSec() / superblock_info_.GetSecsPerZone();
  }
  block_t GetSumBlock(uint32_t segno) const { return ssa_blkaddr_ + segno; }
  uint32_t SitEntryOffset(uint32_t segno) const { return segno % kSitEntryPerBlock; }

  block_t TotalSegs() const { return main_segments_; }

  // GetVictimByDefault() is called for two purposes:
  // 1) One is to select a victim segment for garbage collection, and
  // 2) the other is to find a dirty segment used for SSR.
  // For GC, it tries to find a victim segment that might require less cost
  // to secure free segments among all types of dirty segments.
  // The gc cost can be calculated in two ways according to GcType.
  // In case of GcType::kFgGc, it is typically triggered in the middle of user IO path,
  // and thus it selects a victim with a less valid block count (i.e., GcMode::kGcGreedy)
  // as it hopes the migration completes more quickly.
  // In case of GcType::kBgGc, it is triggered at a idle time,
  // so it uses a cost-benefit method (i.e., GcMode:: kGcCb) rather than kGcGreedy for the victim
  // selection. kGcCb tries to find a cold segment as a victim as it hopes to mitigate a block
  // thrashing problem.
  // Meanwhile, SSR is to reuse invalid blocks for new block allocation, and thus
  // it uses kGcGreedy to select a dirty segment with more invalid blocks
  // among the same type of dirty segments as that of the current segment.
  // If it succeeds in finding an eligible victim, it returns the segment number of the selected
  // victim. If it fails, it returns ZX_ERR_UNAVAILABLE.
  zx::result<uint32_t> GetVictimByDefault(GcType gc_type, CursegType type, AllocMode alloc_mode)
      __TA_EXCLUDES(seglist_lock_) __TA_REQUIRES_SHARED(sentry_lock_);

  zx::result<uint32_t> GetGcVictim(GcType gc_type, CursegType type) __TA_EXCLUDES(sentry_lock_);

  // This function calculates the maximum cost for a victim in each GcType
  // Any segment with a less cost value becomes a victim candidate.
  size_t GetMaxCost(const VictimSelPolicy &policy) const;

  // This method determines GcMode for GetVictimByDefault
  VictimSelPolicy GetVictimSelPolicy(GcType gc_type, CursegType type, AllocMode alloc_mode) const
      __TA_REQUIRES(seglist_lock_);

  uint32_t GetBackgroundVictim() const __TA_REQUIRES_SHARED(seglist_lock_);

  // This method calculates the gc cost for each dirty segment
  size_t GetGcCost(uint32_t segno, const VictimSelPolicy &policy) const
      __TA_REQUIRES_SHARED(sentry_lock_);

  size_t GetCostBenefitRatio(uint32_t segno) const __TA_REQUIRES_SHARED(sentry_lock_);
  zx_status_t DoGarbageCollect(uint32_t segno, GcType gc_type) __TA_REQUIRES(f2fs::GetGlobalLock());

  // for tests and fsck
  void DisableFgGc() { disable_gc_for_test_ = true; }
  void EnableFgGc() { disable_gc_for_test_ = false; }
  bool CanGc() const { return !disable_gc_for_test_; }
  void SetCurVictimSec(uint32_t secno) TA_NO_THREAD_SAFETY_ANALYSIS { cur_victim_sec_ = secno; }
  uint32_t GetCurVictimSec() TA_NO_THREAD_SAFETY_ANALYSIS { return cur_victim_sec_; }
  block_t GetMainSegmentsCount() const { return main_segments_; }
  block_t GetSegmentsCount() const { return segment_count_; }
  SitInfo &GetSitInfo() TA_NO_THREAD_SAFETY_ANALYSIS { return *sit_info_; }
  void SetSitInfo(std::unique_ptr<SitInfo> &&info) TA_NO_THREAD_SAFETY_ANALYSIS {
    sit_info_ = std::move(info);
  }
  FreeSegmapInfo &GetFreeSegmentInfo() TA_NO_THREAD_SAFETY_ANALYSIS { return *free_info_; }
  void SetFreeSegmentInfo(std::unique_ptr<FreeSegmapInfo> &&info) TA_NO_THREAD_SAFETY_ANALYSIS {
    free_info_ = std::move(info);
  }
  DirtySeglistInfo &GetDirtySegmentInfo() TA_NO_THREAD_SAFETY_ANALYSIS { return *dirty_info_; }
  void SetDirtySegmentInfo(std::unique_ptr<DirtySeglistInfo> &&info) TA_NO_THREAD_SAFETY_ANALYSIS {
    dirty_info_ = std::move(info);
  }
  void SetSegmentEntryType(size_t target_segno, CursegType type) TA_NO_THREAD_SAFETY_ANALYSIS {
    sit_info_->sentries[target_segno].type = static_cast<uint8_t>(type);
  }
  uint32_t GetLastVictim(int mode) TA_NO_THREAD_SAFETY_ANALYSIS { return last_victim_[mode]; }
  void SetLastVictim(int mode, uint32_t last_victim) TA_NO_THREAD_SAFETY_ANALYSIS {
    last_victim_[mode] = last_victim;
  }
  void SetSegment0StartBlock(const block_t addr) { seg0_blkaddr_ = addr; }
  void SetMainAreaStartBlock(const block_t addr) { main_blkaddr_ = addr; }
  void SetSSAreaStartBlock(const block_t addr) { ssa_blkaddr_ = addr; }
  void SetSegmentsCount(const block_t count) { segment_count_ = count; }
  void SetMainSegmentsCount(const block_t count) { main_segments_ = count; }
  void SetReservedSegmentsCount(const block_t count) { reserved_segments_ = count; }
  void SetOPSegmentsCount(const block_t count) { ovp_segments_ = count; }

 private:
  friend class F2fsFakeDevTestFixture;
  friend class GcTester;

  zx_status_t GcNodeSegment(const SummaryBlock &sum_blk, uint32_t segno, GcType gc_type)
      __TA_REQUIRES(f2fs::GetGlobalLock());
  // CheckDnode() returns ino of target block and start block index of the target block's dnode
  // block. It also checks the validity of summary.
  zx::result<std::pair<nid_t, block_t>> CheckDnode(const Summary &sum, block_t blkaddr);
  zx_status_t GcDataSegment(const SummaryBlock &sum_blk, unsigned int segno, GcType gc_type)
      __TA_REQUIRES(f2fs::GetGlobalLock());

  uint32_t FindNextInuse(uint32_t max, uint32_t start) __TA_EXCLUDES(segmap_lock_);
  void SetFree(uint32_t segno) __TA_EXCLUDES(segmap_lock_);
  void SetInuse(uint32_t segno) __TA_REQUIRES(segmap_lock_);
  void SetTestAndFree(uint32_t segno) __TA_EXCLUDES(segmap_lock_);
  void SetTestAndInuse(uint32_t segno) __TA_EXCLUDES(segmap_lock_);
  zx::result<> GetSsrSegment(CursegType type) __TA_EXCLUDES(seglist_lock_)
      __TA_REQUIRES(sentry_lock_);
  bool IsCurSeg(uint32_t segno) {
    return (segno == CURSEG_I(CursegType::kCursegHotData)->segno) ||
           (segno == CURSEG_I(CursegType::kCursegWarmData)->segno) ||
           (segno == CURSEG_I(CursegType::kCursegColdData)->segno) ||
           (segno == CURSEG_I(CursegType::kCursegHotNode)->segno) ||
           (segno == CURSEG_I(CursegType::kCursegWarmNode)->segno) ||
           (segno == CURSEG_I(CursegType::kCursegColdNode)->segno);
  }

  bool IsCurSec(uint32_t secno) const {
    return (secno ==
            CURSEG_I(CursegType::kCursegHotData)->segno / superblock_info_.GetSegsPerSec()) ||
           (secno ==
            CURSEG_I(CursegType::kCursegWarmData)->segno / superblock_info_.GetSegsPerSec()) ||
           (secno ==
            CURSEG_I(CursegType::kCursegColdData)->segno / superblock_info_.GetSegsPerSec()) ||
           (secno ==
            CURSEG_I(CursegType::kCursegHotNode)->segno / superblock_info_.GetSegsPerSec()) ||
           (secno ==
            CURSEG_I(CursegType::kCursegWarmNode)->segno / superblock_info_.GetSegsPerSec()) ||
           (secno ==
            CURSEG_I(CursegType::kCursegColdNode)->segno / superblock_info_.GetSegsPerSec());
  }

  // L: Logical segment number in volume, R: Relative segment number in main area
  uint32_t GetL2RSegNo(uint32_t segno) const { return (segno - start_segno_); }
  uint32_t GetR2LSegNo(uint32_t segno) const { return (segno + start_segno_); }
  zx::result<LockedPage> GetCurrentSitPage(uint32_t segno) __TA_REQUIRES(sentry_lock_);
  zx::result<LockedPage> GetNextSitPage(uint32_t start) __TA_REQUIRES(sentry_lock_);

  zx_status_t BuildSitInfo();
  zx_status_t BuildCurseg();
  zx_status_t BuildSitEntries();
  void InitFreeSegmap();
  zx_status_t BuildDirtySegmap() __TA_EXCLUDES(seglist_lock_);
  void InitMinMaxMtime() __TA_EXCLUDES(sentry_lock_);
  void DestroyDirtySegmap() __TA_EXCLUDES(seglist_lock_);

  F2fs *fs_ = nullptr;
  SuperblockInfo &superblock_info_;

  std::shared_mutex sentry_lock_;      // to protect SIT cache
  std::unique_ptr<SitInfo> sit_info_;  // whole segment information

  std::shared_mutex segmap_lock_;  // free segmap lock
  std::unique_ptr<FreeSegmapInfo> free_info_
      __TA_GUARDED(segmap_lock_);  // free segment information

  std::shared_mutex seglist_lock_;  // lock for segment bitmaps
  std::unique_ptr<DirtySeglistInfo> dirty_info_
      __TA_GUARDED(seglist_lock_);          // dirty segment information
  CursegInfo curseg_array_[kNrCursegType];  // active segment information

  block_t start_segno_ = 0;  // start segment number logically

  block_t seg0_blkaddr_ = 0;  // block address of 0'th segment
  block_t main_blkaddr_ = 0;  // start block address of main area
  block_t ssa_blkaddr_ = 0;   // start block address of SSA area

  uint32_t cur_victim_sec_ __TA_GUARDED(seglist_lock_) = kNullSecNo;  // current victim section num
  uint32_t last_victim_[2] __TA_GUARDED(seglist_lock_) = {0};         // last victim segment #
  block_t segment_count_ = 0;                                         // total # of segments
  block_t main_segments_ = 0;                                         // # of segments in main area
  block_t reserved_segments_ = 0;                                     // # of reserved segments
  block_t ovp_segments_ = 0;                                          // # of overprovision segments
  bool disable_gc_for_test_ = false;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_SEGMENT_H_
