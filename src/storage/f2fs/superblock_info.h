// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_SUPERBLOCK_INFO_H_
#define SRC_STORAGE_F2FS_SUPERBLOCK_INFO_H_

#include "src/storage/f2fs/bitmap.h"
#include "src/storage/f2fs/common.h"
#include "src/storage/f2fs/layout.h"
#include "src/storage/f2fs/mount.h"

namespace f2fs {

inline int NatsInCursum(const SummaryBlock &sum) { return LeToCpu(sum.n_nats); }
inline int SitsInCursum(const SummaryBlock &sum) { return LeToCpu(sum.n_sits); }

inline RawNatEntry NatInJournal(const SummaryBlock &sum, int i) { return sum.nat_j.entries[i].ne; }
inline void SetNatInJournal(SummaryBlock &sum, int i, RawNatEntry &raw_ne) {
  sum.nat_j.entries[i].ne = raw_ne;
}
inline nid_t NidInJournal(const SummaryBlock &sum, int i) { return sum.nat_j.entries[i].nid; }
inline void SetNidInJournal(SummaryBlock &sum, int i, nid_t nid) { sum.nat_j.entries[i].nid = nid; }

inline SitEntry &SitInJournal(SummaryBlock &sum, int i) { return sum.sit_j.entries[i].se; }
inline uint32_t SegnoInJournal(const SummaryBlock &sum, int i) {
  return sum.sit_j.entries[i].segno;
}
inline void SetSegnoInJournal(SummaryBlock &sum, int i, uint32_t segno) {
  sum.sit_j.entries[i].segno = segno;
}

// CountType for monitoring
//
// f2fs monitors the number of several block types such as on-writeback,
// dirty dentry blocks, dirty node blocks, and dirty meta blocks.
enum class CountType {
  kWriteback = 0,
  kDirtyDents,
  kDirtyNodes,
  kDirtyMeta,
  kDirtyData,
  kNrCountType,
};

class SuperblockInfo {
 public:
  // Not copyable or moveable
  SuperblockInfo(const SuperblockInfo &) = delete;
  SuperblockInfo &operator=(const SuperblockInfo &) = delete;
  SuperblockInfo(SuperblockInfo &&) = delete;
  SuperblockInfo &operator=(SuperblockInfo &&) = delete;
  SuperblockInfo() = delete;
  SuperblockInfo(std::unique_ptr<Superblock> sb, MountOptions options = {})
      : sb_(std::move(sb)), mount_options_(options), nr_pages_{} {
    InitFromSuperblock();
  }

  bool IsDirty() const { return is_dirty_; }
  void SetDirty() { is_dirty_ = true; }
  void ClearDirty() { is_dirty_ = false; }

  void SetCpFlags(CpFlag flag) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    uint32_t flags = LeToCpu(checkpoint_block_->ckpt_flags);
    flags |= static_cast<uint32_t>(flag);
    checkpoint_block_->ckpt_flags = CpuToLe(flags);
  }

  void ClearCpFlags(CpFlag flag) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    uint32_t flags = LeToCpu(checkpoint_block_->ckpt_flags);
    flags &= (~static_cast<uint32_t>(flag));
    checkpoint_block_->ckpt_flags = CpuToLe(flags);
  }

  bool TestCpFlags(CpFlag flag) __TA_EXCLUDES(mutex_) {
    fs::SharedLock lock(mutex_);
    uint32_t flags = LeToCpu(checkpoint_block_->ckpt_flags);
    return flags & static_cast<uint32_t>(flag);
  }

  const Superblock &GetSuperblock() const { return *sb_; }
  const Checkpoint &GetCheckpoint() const { return *checkpoint_block_; }
  BlockBuffer<Checkpoint> &GetCheckpointBlock() { return checkpoint_block_; }

  zx_status_t SetCheckpoint(const BlockBuffer<Checkpoint> &block) {
    if (zx_status_t status = CheckBlockSize(*block); status != ZX_OK) {
      return status;
    }
    checkpoint_block_ = block;
    InitFromCheckpoint();
    return ZX_OK;
  }

  const RawBitmap &GetExtraSitBitmap() const { return extra_sit_bitmap_; }
  RawBitmap &GetExtraSitBitmap() { return extra_sit_bitmap_; }
  void SetExtraSitBitmap(size_t num_bytes) { extra_sit_bitmap_.Reset(GetBitSize(num_bytes)); }

  // superblock fields
  block_t GetLogSectorsPerBlock() const { return log_sectors_per_block_; }
  block_t GetLogBlocksize() const { return log_blocksize_; }
  block_t GetBlocksize() const { return blocksize_; }
  uint32_t GetRootIno() const { return root_ino_num_; }
  uint32_t GetNodeIno() const { return node_ino_num_; }
  uint32_t GetMetaIno() const { return meta_ino_num_; }
  block_t GetLogBlocksPerSeg() const { return log_blocks_per_seg_; }
  block_t GetBlocksPerSeg() const { return blocks_per_seg_; }
  block_t GetSegsPerSec() const { return segs_per_sec_; }
  block_t GetSecsPerZone() const { return secs_per_zone_; }
  block_t GetTotalSections() const { return total_sections_; }

  // for test
  void SetTotalBlockCount(block_t block_count) { total_block_count_ = block_count; }
  void SetValidBlockCount(block_t block_count) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    valid_block_count_ = block_count;
  }
  void SetValidNodeCount(nid_t block_count) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    valid_node_count_ = block_count;
  }

  // for space status
  block_t GetValidBlockCount() __TA_EXCLUDES(mutex_) {
    fs::SharedLock lock(mutex_);
    return valid_block_count_;
  }
  nid_t GetMaxNodeCount() const { return max_node_count_; }
  block_t GetTotalBlockCount() const { return total_block_count_; }

  void ResetAllocBlockCount() __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    last_valid_block_count_ = valid_block_count_;
    alloc_block_count_ = 0;
  }

  bool SpaceForRollForward() __TA_EXCLUDES(mutex_) {
    fs::SharedLock lock(mutex_);
    return last_valid_block_count_ + alloc_block_count_ <= total_block_count_;
  }

  bool IncValidNodeCount(block_t count) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    block_t valid_block_count = valid_block_count_ + count;
    block_t valid_node_count = valid_node_count_ + count;

    if (valid_block_count > total_block_count_ || valid_node_count > max_node_count_) {
      return false;
    }
    alloc_block_count_ += count;
    valid_node_count_ = valid_node_count;
    valid_block_count_ = valid_block_count;

    return true;
  }

  void DecValidNodeCount(uint32_t count) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    ZX_ASSERT(valid_block_count_ >= count);
    ZX_ASSERT(valid_node_count_ >= count);
    valid_node_count_ -= count;
    valid_block_count_ -= count;
  }
  nid_t GetValidNodeCount() __TA_EXCLUDES(mutex_) {
    fs::SharedLock lock(mutex_);
    return valid_node_count_;
  }
  nid_t GetValidInodeCount() __TA_EXCLUDES(mutex_) {
    fs::SharedLock lock(mutex_);
    return valid_inode_count_;
  }
  void DecValidInodeCount() __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    ZX_ASSERT(valid_inode_count_);
    --valid_inode_count_;
  }
  void IncValidInodeCount() __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    ZX_ASSERT(valid_inode_count_ != max_node_count_);
    ++valid_inode_count_;
  }
  void DecValidBlockCount(block_t count) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    ZX_ASSERT(valid_block_count_ >= count);
    valid_block_count_ -= count;
  }
  zx_status_t IncValidBlockCount(block_t count) __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    if (valid_block_count_ + count > total_block_count_) {
      return ZX_ERR_NO_SPACE;
    }
    valid_block_count_ += count;
    alloc_block_count_ += count;
    return ZX_OK;
  }

  uint32_t Utilization() __TA_EXCLUDES(mutex_) {
    fs::SharedLock lock(mutex_);
    return 100 * valid_block_count_ / total_block_count_;
  }

  uint32_t GetNextGeneration() const { return s_next_generation_; }
  void IncNextGeneration() { ++s_next_generation_; }

  const std::vector<std::string> &GetExtensionList() const { return extension_list_; }
  size_t GetActiveLogs() const {
    auto ret = mount_options_.GetValue(MountOption::kActiveLogs);
    ZX_DEBUG_ASSERT(ret.is_ok());
    return *ret;
  }
  void ClearOpt(MountOption option) { mount_options_.SetValue(option, 0); }
  void SetOpt(MountOption option) { mount_options_.SetValue(option, 1); }
  bool TestOpt(MountOption option) const {
    if (auto ret = mount_options_.GetValue(option); ret.is_ok() && *ret) {
      return true;
    }
    return false;
  }

  void IncSegmentCount(int type) { ++segment_count_[type]; }
  uint64_t GetSegmentCount(int type) const { return segment_count_[type]; }
  void IncBlockCount(int type) { ++block_count_[type]; }

  void IncreasePageCount(CountType count_type) {
    // Use release-acquire ordering with nr_pages_.
    atomic_fetch_add_explicit(&nr_pages_[static_cast<int>(count_type)], 1,
                              std::memory_order_release);
    SetDirty();
  }
  void DecreasePageCount(CountType count_type) {
    // Use release-acquire ordering with nr_pages_.
    atomic_fetch_sub_explicit(&nr_pages_[static_cast<int>(count_type)], 1,
                              std::memory_order_release);
  }
  int GetPageCount(CountType count_type) const {
    // Use release-acquire ordering with nr_pages_.
    return atomic_load_explicit(&nr_pages_[static_cast<int>(count_type)],
                                std::memory_order_acquire);
  }

  void IncreaseDirtyDir() { ++n_dirty_dirs; }
  void DecreaseDirtyDir() { --n_dirty_dirs; }

  // in byte
  uint32_t GetSitBitmapSize() const { return LeToCpu(checkpoint_block_->sit_ver_bitmap_bytesize); }
  uint32_t GetNatBitmapSize() const { return LeToCpu(checkpoint_block_->nat_ver_bitmap_bytesize); }
  uint8_t *GetNatBitmap() {
    if (cp_payload_ > 0) {
      return checkpoint_block_->sit_nat_version_bitmap;
    }
    return checkpoint_block_->sit_nat_version_bitmap + GetSitBitmapSize();
  }
  uint8_t *GetSitBitmap() {
    if (cp_payload_ > 0) {
      return static_cast<uint8_t *>(GetExtraSitBitmap().StorageUnsafe()->GetData());
    }
    return checkpoint_block_->sit_nat_version_bitmap;
  }

  block_t StartCpAddr() const {
    block_t start_addr;
    uint64_t ckpt_version = LeToCpu(checkpoint_block_->checkpoint_ver);
    start_addr = LeToCpu(sb_->cp_blkaddr);

    // odd numbered checkpoint should at cp segment 0
    // and even segent must be at cp segment 1
    if (!(ckpt_version & 1)) {
      start_addr += blocks_per_seg_;
    }
    return start_addr;
  }
  block_t StartSumAddr() const { return LeToCpu(checkpoint_block_->cp_pack_start_sum); }
  block_t GetNumCpPayload() const { return cp_payload_; }

  zx::result<uint32_t> GetCrcFromCheckpointBlock(
      std::optional<const Checkpoint *> block = std::nullopt) {
    const Checkpoint *cp_block = block ? *block : checkpoint_block_.get<Checkpoint>();
    size_t offset = LeToCpu(cp_block->checksum_offset);
    if (offset >= kBlockSize) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    return zx::ok(
        *reinterpret_cast<const uint32_t *>(reinterpret_cast<const uint8_t *>(cp_block) + offset));
  }

  uint64_t GetCheckpointVer(bool with_crc = false) {
    uint64_t version = checkpoint_ver_;
    if (with_crc && TestCpFlags(CpFlag::kCpCrcRecoveryFlag)) {
      zx::result crc_or = GetCrcFromCheckpointBlock();
      ZX_DEBUG_ASSERT(crc_or.is_ok());
      version |= static_cast<uint64_t>(*crc_or) << 32;
    }
    return version;
  }
  void UpdateCheckpointVer() { checkpoint_block_->checkpoint_ver = CpuToLe(++checkpoint_ver_); }

 private:
  void InitFromSuperblock() {
    log_sectors_per_block_ = LeToCpu(sb_->log_sectors_per_block);
    log_blocksize_ = LeToCpu(sb_->log_blocksize);
    blocksize_ = 1 << GetLogBlocksize();
    log_blocks_per_seg_ = LeToCpu(sb_->log_blocks_per_seg);
    blocks_per_seg_ = 1 << GetLogBlocksPerSeg();
    segs_per_sec_ = LeToCpu(sb_->segs_per_sec);
    secs_per_zone_ = LeToCpu(sb_->secs_per_zone);
    total_sections_ = LeToCpu(sb_->section_count);
    max_node_count_ = LeToCpu(sb_->segment_count_nat) / 2 * GetBlocksPerSeg() * kNatEntryPerBlock;
    root_ino_num_ = LeToCpu(sb_->root_ino);
    node_ino_num_ = LeToCpu(sb_->node_ino);
    meta_ino_num_ = LeToCpu(sb_->meta_ino);
    cp_payload_ = LeToCpu(sb_->cp_payload);

    ZX_ASSERT(sb_->extension_count < kMaxExtension);
    for (size_t index = 0; index < sb_->extension_count; ++index) {
      ZX_ASSERT(sb_->extension_list[index][7] == '\0');
      extension_list_.push_back(reinterpret_cast<char *>(sb_->extension_list[index]));
    }
    ZX_ASSERT(sb_->extension_count == extension_list_.size());
  }

  void InitFromCheckpoint() __TA_EXCLUDES(mutex_) {
    std::lock_guard lock(mutex_);
    valid_node_count_ = LeToCpu(checkpoint_block_->valid_node_count);
    valid_inode_count_ = LeToCpu(checkpoint_block_->valid_inode_count);
    total_block_count_ = static_cast<block_t>(checkpoint_block_->user_block_count);
    last_valid_block_count_ = valid_block_count_ =
        static_cast<block_t>(LeToCpu(checkpoint_block_->valid_block_count));
    alloc_block_count_ = 0;
    checkpoint_ver_ = LeToCpu(checkpoint_block_->checkpoint_ver);
  }

  zx_status_t CheckBlockSize(const Checkpoint &ckpt) const {
    size_t total = LeToCpu(sb_->segment_count);
    size_t fsmeta = LeToCpu(sb_->segment_count_ckpt);
    fsmeta += LeToCpu(sb_->segment_count_sit);
    fsmeta += LeToCpu(sb_->segment_count_nat);
    fsmeta += LeToCpu(ckpt.rsvd_segment_count);
    fsmeta += LeToCpu(sb_->segment_count_ssa);
    if (fsmeta >= total) {
      return ZX_ERR_BAD_STATE;
    }

    size_t sit_ver_bitmap_bytesize =
        ((LeToCpu(sb_->segment_count_sit) / 2) << LeToCpu(sb_->log_blocks_per_seg)) / 8;
    size_t nat_ver_bitmap_bytesize =
        ((LeToCpu(sb_->segment_count_nat) / 2) << LeToCpu(sb_->log_blocks_per_seg)) / 8;
    block_t nat_blocks = (LeToCpu(sb_->segment_count_nat) >> 1) << LeToCpu(sb_->log_blocks_per_seg);

    if (LeToCpu(ckpt.sit_ver_bitmap_bytesize) != sit_ver_bitmap_bytesize ||
        LeToCpu(ckpt.nat_ver_bitmap_bytesize) != nat_ver_bitmap_bytesize ||
        LeToCpu(ckpt.next_free_nid) >= kNatEntryPerBlock * nat_blocks) {
      return ZX_ERR_BAD_STATE;
    }

    return ZX_OK;
  }

  std::unique_ptr<Superblock> sb_;
  bool is_dirty_ = false;  // dirty flag for checkpoint

  BlockBuffer<Checkpoint> checkpoint_block_;

  RawBitmap extra_sit_bitmap_;

  MountOptions mount_options_;
  uint64_t n_dirty_dirs = 0;                          // # of dir inodes
  block_t log_sectors_per_block_ = 0;                 // log2 sectors per block
  block_t log_blocksize_ = 0;                         // log2 block size
  block_t blocksize_ = 0;                             // block size
  nid_t root_ino_num_ = 0;                            // root inode number
  nid_t node_ino_num_ = 0;                            // node inode number
  nid_t meta_ino_num_ = 0;                            // meta inode number
  block_t log_blocks_per_seg_ = 0;                    // log2 blocks per segment
  block_t blocks_per_seg_ = 0;                        // blocks per segment
  block_t segs_per_sec_ = 0;                          // segments per section
  block_t secs_per_zone_ = 0;                         // sections per zone
  block_t total_sections_ = 0;                        // total section count
  nid_t max_node_count_ = 0;                          // maximum number of node blocks
  nid_t valid_node_count_ __TA_GUARDED(mutex_) = 0;   // valid node block count
  nid_t valid_inode_count_ __TA_GUARDED(mutex_) = 0;  // valid inode count

  block_t cp_payload_ = 0;
  block_t total_block_count_ = 0;                            // # of user blocks
  block_t valid_block_count_ __TA_GUARDED(mutex_) = 0;       // # of valid blocks
  block_t alloc_block_count_ __TA_GUARDED(mutex_) = 0;       // # of allocated blocks
  block_t last_valid_block_count_ __TA_GUARDED(mutex_) = 0;  // for recovery
  uint32_t s_next_generation_ = 0;                           // for NFS support
  std::atomic<int> nr_pages_[static_cast<int>(CountType::kNrCountType)] = {
      0};  // # of pages, see count_type

  uint64_t checkpoint_ver_ = 0;
  uint64_t segment_count_[2] = {0};  // # of allocated segments
  uint64_t block_count_[2] = {0};    // # of allocated blocks

  std::vector<std::string> extension_list_;

  std::shared_mutex mutex_;  // for checkpoint data
};

#if 0  // porting needed
[[maybe_unused]] static inline void SetAclInode(InodeInfo *fi, umode_t mode) {
  fi->i_acl_mode = mode;
  SetInodeFlag(fi, InodeInfoFlag::kAclMode);
}

[[maybe_unused]] static inline int CondClearInodeFlag(InodeInfo *fi, InodeInfoFlag flag) {
  if (IsInodeFlagSet(fi, InodeInfoFlag::kAclMode)) {
    ClearInodeFlag(fi, InodeInfoFlag::kAclMode);
    return 1;
  }
  return 0;
}
#endif

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_SUPERBLOCK_INFO_H_
