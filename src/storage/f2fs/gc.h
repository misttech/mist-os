// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_GC_H_
#define SRC_STORAGE_F2FS_GC_H_

namespace f2fs {

class GcManager {
 public:
  GcManager(const GcManager &) = delete;
  GcManager &operator=(const GcManager &) = delete;
  GcManager(GcManager &&) = delete;
  GcManager &operator=(GcManager &&) = delete;
  GcManager() = delete;
  GcManager(F2fs *fs);

  zx::result<uint32_t> Run() __TA_EXCLUDES(f2fs::GetGlobalLock());

  // For testing
  void DisableFgGc() { disable_gc_for_test_ = true; }
  void EnableFgGc() { disable_gc_for_test_ = false; }

 private:
  friend class GcTester;
  zx_status_t DoGarbageCollect(uint32_t segno, GcType gc_type) __TA_REQUIRES(f2fs::GetGlobalLock());

  zx_status_t GcNodeSegment(const SummaryBlock &sum_blk, uint32_t segno, GcType gc_type)
      __TA_REQUIRES(f2fs::GetGlobalLock());

  // CheckDnode() returns ino of target block and start block index of the target block's dnode
  // block. It also checks the validity of summary.
  zx::result<std::pair<nid_t, block_t>> CheckDnode(const Summary &sum, block_t blkaddr);
  zx_status_t GcDataSegment(const SummaryBlock &sum_blk, unsigned int segno, GcType gc_type)
      __TA_REQUIRES(f2fs::GetGlobalLock());

  F2fs *fs_ = nullptr;
  SuperblockInfo &superblock_info_;
  uint32_t cur_victim_sec_ = kNullSecNo;  // current victim section num
  std::binary_semaphore run_{1};
  SegmentManager &segment_manager_;

  // For testing
  bool disable_gc_for_test_ = false;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_GC_H_
