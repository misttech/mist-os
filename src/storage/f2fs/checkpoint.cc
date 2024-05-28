// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

zx_status_t F2fs::GrabMetaPage(pgoff_t index, LockedPage *out) {
  if (zx_status_t ret = GetMetaVnode().GrabCachePage(index, out); ret != ZX_OK) {
    ZX_ASSERT_MSG(false, "GrabMetaPage() fails [addr: 0x%lx, ret: %d]\n", index, ret);
    return ret;
  }
  // We wait writeback only inside GrabMetaPage()
  (*out)->WaitOnWriteback();
  (*out)->SetUptodate();
  return ZX_OK;
}

zx_status_t F2fs::GetMetaPage(pgoff_t index, LockedPage *out) {
  LockedPage page;
  if (zx_status_t ret = GetMetaVnode().GrabCachePage(index, &page); ret != ZX_OK) {
    ZX_ASSERT_MSG(false, "GetMetaPage() fails [addr: 0x%lx, ret: %d]\n", index, ret);
    return ret;
  }

  if (auto status =
          MakeReadOperation(page, safemath::checked_cast<block_t>(index), PageType::kMeta);
      status.is_error()) {
    return status.status_value();
  }
#if 0  // porting needed
  // mark_page_accessed(page);
#endif
  *out = std::move(page);
  return ZX_OK;
}

pgoff_t F2fs::FlushDirtyMetaPages(bool is_commit) {
  if (superblock_info_->GetPageCount(CountType::kDirtyMeta) == 0) {
    return 0;
  }
  WritebackOperation operation;
  if (is_commit) {
    operation.bSync = true;
    operation.page_cb = [](fbl::RefPtr<Page> page, bool is_last_page) {
      if (is_last_page) {
        page->SetCommit();
      }
      return ZX_OK;
    };
    sync_completion_t completion;
    ScheduleWriter(&completion);
    ZX_ASSERT_MSG(sync_completion_wait(&completion, zx::sec(kWriteTimeOut).get()) == ZX_OK,
                  "FlushDirtyMetaPages() timeout");
  }
  return GetMetaVnode().Writeback(operation);
}

zx_status_t F2fs::CheckOrphanSpace() {
  /*
   * considering 512 blocks in a segment 5 blocks are needed for cp
   * and log segment summaries. Remaining blocks are used to keep
   * orphan entries with the limitation one reserved segment
   * for cp pack we can have max 1020*507 orphan entries
   */
  size_t max_orphans = (superblock_info_->GetBlocksPerSeg() - 5) * kOrphansPerBlock;
  if (GetVnodeSetSize(VnodeSet::kOrphan) >= max_orphans) {
    inspect_tree_->OnOutOfSpace();
    return ZX_ERR_NO_SPACE;
  }
  return ZX_OK;
}

void F2fs::PurgeOrphanInode(nid_t ino) {
  fbl::RefPtr<VnodeF2fs> vnode;
  ZX_ASSERT(VnodeF2fs::Vget(this, ino, &vnode) == ZX_OK);
  vnode->ClearNlink();
  // truncate all the data and nodes in VnodeF2fs::Recycle()
}

zx_status_t F2fs::PurgeOrphanInodes() {
  if (!(superblock_info_->TestCpFlags(CpFlag::kCpOrphanPresentFlag))) {
    return ZX_OK;
  }
  SetOnRecovery();
  block_t start_blk = superblock_info_->StartCpAddr() + superblock_info_->GetNumCpPayload() + 1;
  block_t orphan_blkaddr = superblock_info_->StartSumAddr() - 1;

  for (block_t i = 0; i < orphan_blkaddr; ++i) {
    LockedPage page;
    if (zx_status_t ret = GetMetaPage(start_blk + i, &page); ret != ZX_OK) {
      return ret;
    }

    OrphanBlock *orphan_blk;

    orphan_blk = page->GetAddress<OrphanBlock>();
    uint32_t entry_count = LeToCpu(orphan_blk->entry_count);
    // TODO: Need to set NeedChkp flag to repair the fs when fsck repair is available.
    // For now, we trigger assertion.
    ZX_ASSERT(entry_count <= kOrphansPerBlock);
    for (block_t j = 0; j < entry_count; ++j) {
      nid_t ino = LeToCpu(orphan_blk->ino[j]);
      PurgeOrphanInode(ino);
    }
  }
  // clear Orphan Flag
  superblock_info_->ClearCpFlags(CpFlag::kCpOrphanPresentFlag);
  ClearOnRecovery();
  return ZX_OK;
}

void F2fs::WriteOrphanInodes(block_t start_blk) {
  OrphanBlock *orphan_blk = nullptr;
  LockedPage page;
  uint32_t nentries = 0;
  uint16_t index = 1;
  uint16_t orphan_blocks;

  orphan_blocks = static_cast<uint16_t>(
      (GetVnodeSetSize(VnodeSet::kOrphan) + (kOrphansPerBlock - 1)) / kOrphansPerBlock);

  ForAllVnodeSet(VnodeSet::kOrphan, [&](nid_t ino) {
    if (nentries == kOrphansPerBlock) {
      // an orphan block is full of 1020 entries,
      // then we need to flush current orphan blocks
      // and bring another one in memory
      orphan_blk->blk_addr = CpuToLe(index);
      orphan_blk->blk_count = CpuToLe(orphan_blocks);
      orphan_blk->entry_count = CpuToLe(nentries);
      page.SetDirty();
      page.reset();
      ++index;
      ++start_blk;
      nentries = 0;
    }
    if (!page) {
      GrabMetaPage(start_blk, &page);
      orphan_blk = page->GetAddress<OrphanBlock>();
      memset(orphan_blk, 0, sizeof(*orphan_blk));
      page.SetDirty();
    }
    orphan_blk->ino[nentries++] = CpuToLe(ino);
  });
  if (page) {
    orphan_blk->blk_addr = CpuToLe(index);
    orphan_blk->blk_count = CpuToLe(orphan_blocks);
    orphan_blk->entry_count = CpuToLe(nentries);
    page.SetDirty();
  }
}

zx_status_t F2fs::ValidateCheckpoint(block_t cp_addr, uint64_t *version, LockedPage *out) {
  LockedPage cp_page_1, cp_page_2;
  uint64_t blk_size = superblock_info_->GetBlocksize();
  Checkpoint *cp_block;
  uint64_t cur_version = 0, pre_version = 0;
  uint32_t crc = 0;
  uint32_t crc_offset;

  // Read the 1st cp block in this CP pack
  if (zx_status_t ret = GetMetaPage(cp_addr, &cp_page_1); ret != ZX_OK) {
    return ret;
  }

  // get the version number
  cp_block = cp_page_1->GetAddress<Checkpoint>();
  crc_offset = LeToCpu(cp_block->checksum_offset);
  if (crc_offset >= blk_size) {
    return ZX_ERR_BAD_STATE;
  }

  crc = *reinterpret_cast<uint32_t *>(reinterpret_cast<uint8_t *>(cp_block) + crc_offset);
  if (!F2fsCrcValid(crc, cp_block, crc_offset)) {
    return ZX_ERR_BAD_STATE;
  }

  pre_version = LeToCpu(cp_block->checkpoint_ver);

  // Read the 2nd cp block in this CP pack
  cp_addr += LeToCpu(cp_block->cp_pack_total_block_count) - 1;
  if (zx_status_t ret = GetMetaPage(cp_addr, &cp_page_2); ret != ZX_OK) {
    return ret;
  }

  cp_block = cp_page_2->GetAddress<Checkpoint>();
  crc_offset = LeToCpu(cp_block->checksum_offset);
  if (crc_offset >= blk_size) {
    return ZX_ERR_BAD_STATE;
  }

  crc = *reinterpret_cast<uint32_t *>(reinterpret_cast<uint8_t *>(cp_block) + crc_offset);
  if (!F2fsCrcValid(crc, cp_block, crc_offset)) {
    return ZX_ERR_BAD_STATE;
  }

  cur_version = LeToCpu(cp_block->checkpoint_ver);

  if (cur_version == pre_version) {
    *version = cur_version;
    *out = std::move(cp_page_1);
    return ZX_OK;
  }
  return ZX_ERR_BAD_STATE;
}

zx_status_t F2fs::GetValidCheckpoint() {
  LockedPage cp1, cp2;
  fbl::RefPtr<Page> cur_page;
  uint64_t cp1_version = 0, cp2_version = 0;
  block_t cp_start_blk_no;

  /*
   * Finding out valid cp block involves read both
   * sets( cp pack1 and cp pack 2)
   */
  block_t start_addr = LeToCpu(superblock_info_->GetSuperblock().cp_blkaddr);
  cp_start_blk_no = start_addr;
  ValidateCheckpoint(cp_start_blk_no, &cp1_version, &cp1);

  /* The second checkpoint pack should start at the next segment */
  cp_start_blk_no += 1 << superblock_info_->GetLogBlocksPerSeg();
  ValidateCheckpoint(cp_start_blk_no, &cp2_version, &cp2);

  if (cp1 && cp2) {
    if (VerAfter(cp2_version, cp1_version)) {
      if (superblock_info_->SetCheckpoint(cp2.CopyRefPtr()) != ZX_OK) {
        cur_page = cp1.CopyRefPtr();
        cp_start_blk_no = start_addr;
      }
    } else {
      if (superblock_info_->SetCheckpoint(cp1.CopyRefPtr()) != ZX_OK) {
        cur_page = cp2.CopyRefPtr();
      } else {
        cp_start_blk_no = start_addr;
      }
    }
  } else if (cp1) {
    cur_page = cp1.CopyRefPtr();
    cp_start_blk_no = start_addr;
  } else if (cp2) {
    cur_page = cp2.CopyRefPtr();
  } else {
    return ZX_ERR_INVALID_ARGS;
  }

  if (cur_page) {
    if (zx_status_t status = superblock_info_->SetCheckpoint(cur_page); status != ZX_OK) {
      return status;
    }
  }

  block_t num_payload = superblock_info_->GetNumCpPayload();
  if (num_payload) {
    size_t blk_size = superblock_info_->GetBlocksize();
    superblock_info_->SetExtraSitBitmap(num_payload * blk_size);
    for (uint32_t i = 0; i < num_payload; ++i) {
      LockedPage cp_page;
      if (zx_status_t ret = GetMetaPage(cp_start_blk_no + 1 + i, &cp_page); ret != ZX_OK) {
        return ret;
      }
      CloneBits(superblock_info_->GetExtraSitBitmap(), cp_page->GetAddress(),
                GetBitSize(i * blk_size), GetBitSize(blk_size));
    }
  }

  return ZX_OK;
}

pgoff_t F2fs::FlushDirtyDataPages(WritebackOperation &operation, bool wait_writer) {
  pgoff_t total_nwritten = 0;
  GetVCache().ForDirtyVnodesIf(
      [&](fbl::RefPtr<VnodeF2fs> &vnode) {
        if (!vnode->IsValid()) {
          ZX_ASSERT(vnode->ClearDirty());
        } else if (vnode->GetDirtyPageCount()) {
          auto nwritten = vnode->Writeback(operation);
          total_nwritten = safemath::CheckAdd<pgoff_t>(total_nwritten, nwritten).ValueOrDie();
          if (nwritten >= operation.to_write) {
            return ZX_ERR_STOP;
          }
          operation.to_write -= nwritten;
        }
        return ZX_OK;
      },
      std::move(operation.if_vnode));

  if (wait_writer) {
    // Wait until writers finish and release the ref of dirty vnodes.
    sync_completion_t completion;
    ScheduleWriter(&completion);
    ZX_ASSERT_MSG(sync_completion_wait(&completion, zx::sec(kWriteTimeOut).get()) == ZX_OK,
                  "FlushDirtyDataPages() timeout");
  }
  return total_nwritten;
}

zx::result<pgoff_t> F2fs::FlushDirtyNodePages(WritebackOperation &operation) {
  if (zx_status_t status = GetVCache().ForDirtyVnodesIf(
          [](fbl::RefPtr<VnodeF2fs> &vnode) {
            ZX_ASSERT(vnode->IsValid());
            ZX_ASSERT(vnode->ClearDirty());
            return ZX_OK;
          },
          [this](fbl::RefPtr<VnodeF2fs> &vnode) {
            if (vnode->GetDirtyPageCount()) {
              return ZX_ERR_NEXT;
            }
            // Update the node page of every dirty vnode before writeback.
            LockedPage node_page;
            if (zx_status_t ret = node_manager_->GetNodePage(vnode->GetKey(), &node_page);
                ret != ZX_OK) {
              return ret;
            }
            vnode->UpdateInodePage(node_page, true);
            return ZX_OK;
          });
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(GetNodeVnode().Writeback(operation));
}

void F2fs::FlushDirsAndNodes() {
  do {
    // Write out all dirty dentry pages and remove orphans from dirty list.
    WritebackOperation op;
    op.if_vnode = [](fbl::RefPtr<VnodeF2fs> &vnode) {
      if (vnode->IsDir() || !vnode->IsValid()) {
        return ZX_OK;
      }
      return ZX_ERR_NEXT;
    };
    FlushDirtyDataPages(op, true);
  } while (superblock_info_->GetPageCount(CountType::kDirtyDents));

  // POR: we should ensure that there is no dirty node pages
  // until finishing nat/sit flush.
  do {
    WritebackOperation op;
    auto ret = FlushDirtyNodePages(op);
    ZX_ASSERT_MSG(ret.is_ok(), "Failed to flush node pages (%ul) %s",
                  superblock_info_->GetPageCount(CountType::kDirtyNodes), ret.status_string());
  } while (superblock_info_->GetPageCount(CountType::kDirtyNodes));
}

zx_status_t F2fs::DoCheckpoint(bool is_umount) {
  // Flush all the NAT/SIT pages
  SuperblockInfo &superblock_info = GetSuperblockInfo();
  while (superblock_info.GetPageCount(CountType::kDirtyMeta)) {
    FlushDirtyMetaPages(false);
  }

  ScheduleWriter();

  auto &ckpt_block = superblock_info.GetCheckpointBlock();
  if (auto last_nid_or = GetNodeManager().GetNextFreeNid(); last_nid_or.is_ok()) {
    ckpt_block->next_free_nid = CpuToLe(*last_nid_or);
  } else {
    ckpt_block->next_free_nid = CpuToLe(GetNodeManager().GetNextScanNid());
  }

  // modify checkpoint
  // version number is already updated
  ckpt_block->elapsed_time = CpuToLe(GetSegmentManager().GetMtime());
  ckpt_block->valid_block_count = CpuToLe(superblock_info.GetValidBlockCount());
  ckpt_block->free_segment_count = CpuToLe(GetSegmentManager().FreeSegments());
  for (int i = 0; i < 3; ++i) {
    ckpt_block->cur_node_segno[i] =
        CpuToLe(GetSegmentManager().CursegSegno(i + static_cast<int>(CursegType::kCursegHotNode)));
    ckpt_block->cur_node_blkoff[i] =
        CpuToLe(GetSegmentManager().CursegBlkoff(i + static_cast<int>(CursegType::kCursegHotNode)));
    ckpt_block->alloc_type[i + static_cast<int>(CursegType::kCursegHotNode)] =
        GetSegmentManager().CursegAllocType(i + static_cast<int>(CursegType::kCursegHotNode));
  }
  for (int i = 0; i < 3; ++i) {
    ckpt_block->cur_data_segno[i] =
        CpuToLe(GetSegmentManager().CursegSegno(i + static_cast<int>(CursegType::kCursegHotData)));
    ckpt_block->cur_data_blkoff[i] =
        CpuToLe(GetSegmentManager().CursegBlkoff(i + static_cast<int>(CursegType::kCursegHotData)));
    ckpt_block->alloc_type[i + static_cast<int>(CursegType::kCursegHotData)] =
        GetSegmentManager().CursegAllocType(i + static_cast<int>(CursegType::kCursegHotData));
  }

  ckpt_block->valid_node_count = CpuToLe(superblock_info.GetValidNodeCount());
  ckpt_block->valid_inode_count = CpuToLe(superblock_info.GetValidInodeCount());

  // 2 cp  + n data seg summary + orphan inode blocks
  uint32_t data_sum_blocks = GetSegmentManager().NpagesForSummaryFlush();
  if (data_sum_blocks < 3) {
    superblock_info.SetCpFlags(CpFlag::kCpCompactSumFlag);
  } else {
    superblock_info.ClearCpFlags(CpFlag::kCpCompactSumFlag);
  }

  block_t num_cp_payload = superblock_info.GetNumCpPayload();
  uint32_t orphan_blocks = static_cast<uint32_t>(
      (GetVnodeSetSize(VnodeSet::kOrphan) + kOrphansPerBlock - 1) / kOrphansPerBlock);
  uint32_t cp_pack_total_block_count = 2 + data_sum_blocks + orphan_blocks + num_cp_payload;
  if (is_umount) {
    superblock_info.SetCpFlags(CpFlag::kCpUmountFlag);
    cp_pack_total_block_count += kNrCursegNodeType;
  } else {
    superblock_info.ClearCpFlags(CpFlag::kCpUmountFlag);
  }
  ckpt_block->cp_pack_start_sum = CpuToLe(1 + orphan_blocks + num_cp_payload);
  ckpt_block->cp_pack_total_block_count = CpuToLe(cp_pack_total_block_count);

  if (GetVnodeSetSize(VnodeSet::kOrphan) > 0) {
    superblock_info.SetCpFlags(CpFlag::kCpOrphanPresentFlag);
  } else {
    superblock_info.ClearCpFlags(CpFlag::kCpOrphanPresentFlag);
  }

  // update SIT/NAT bitmap
  GetSegmentManager().GetSitBitmap(superblock_info.GetSitBitmap());
  GetNodeManager().GetNatBitmap(superblock_info.GetNatBitmap());

  uint32_t crc32 = CpuToLe(F2fsCrc32(&ckpt_block, LeToCpu(ckpt_block->checksum_offset)));
  std::memcpy(ckpt_block.get<uint8_t>() + LeToCpu(ckpt_block->checksum_offset), &crc32,
              sizeof(uint32_t));

  block_t start_blk = superblock_info.StartCpAddr();

  // Prepare Pages for this checkpoint pack
  {
    LockedPage cp_page;
    GrabMetaPage(start_blk++, &cp_page);
    cp_page->Write(&ckpt_block, 0, superblock_info.GetBlocksize());
    cp_page.SetDirty();
  }

  size_t offset = 0;
  for (size_t i = 0; i < num_cp_payload; ++i) {
    LockedPage cp_page;
    GrabMetaPage(start_blk++, &cp_page);
    memcpy(cp_page->GetAddress(), &superblock_info.GetSitBitmap()[offset], cp_page->Size());
    cp_page.SetDirty();
    offset += cp_page->Size();
  }

  if (GetVnodeSetSize(VnodeSet::kOrphan) > 0) {
    WriteOrphanInodes(start_blk);
    start_blk += orphan_blocks;
  }

  GetSegmentManager().WriteDataSummaries(start_blk);
  start_blk += data_sum_blocks;
  if (is_umount) {
    GetSegmentManager().WriteNodeSummaries(start_blk);
    start_blk += kNrCursegNodeType;
  }

  // Write out this checkpoint pack.
  FlushDirtyMetaPages(false);

  // Prepare the commit block.
  {
    LockedPage cp_page;
    GrabMetaPage(start_blk, &cp_page);
    cp_page->Write(&ckpt_block, 0, superblock_info.GetBlocksize());
    cp_page.SetDirty();
  }

  // Update the valid block count.
  superblock_info.ResetAllocBlockCount();

  // Commit.
  if (!superblock_info.TestCpFlags(CpFlag::kCpErrorFlag)) {
    ZX_ASSERT(superblock_info.GetPageCount(CountType::kDirtyMeta) == 1);
    FlushDirtyMetaPages(true);
    if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
      return ZX_ERR_UNAVAILABLE;
    }
    GetSegmentManager().ClearPrefreeSegments();
    superblock_info.ClearDirty();
    meta_vnode_->InvalidatePages();
    ClearVnodeSet();
  }
  return ZX_OK;
}

uint32_t F2fs::GetFreeSectionsForDirtyPages() {
  uint32_t pages_per_sec =
      safemath::CheckMul<uint32_t>((1 << superblock_info_->GetLogBlocksPerSeg()),
                                   superblock_info_->GetSegsPerSec())
          .ValueOrDie();
  uint32_t node_secs =
      safemath::CheckDiv<uint32_t>(
          ((superblock_info_->GetPageCount(CountType::kDirtyNodes) + pages_per_sec - 1) >>
           superblock_info_->GetLogBlocksPerSeg()),
          superblock_info_->GetSegsPerSec())
          .ValueOrDie();
  uint32_t dent_secs =
      safemath::CheckDiv<uint32_t>(
          ((superblock_info_->GetPageCount(CountType::kDirtyDents) + pages_per_sec - 1) >>
           superblock_info_->GetLogBlocksPerSeg()),
          superblock_info_->GetSegsPerSec())
          .ValueOrDie();

  return (node_secs + safemath::CheckMul<uint32_t>(dent_secs, 2)).ValueOrDie();
}

// Release-acquire ordering between the writeback (loader) and others such as checkpoint and gc.
bool F2fs::CanReclaim() const { return !stop_reclaim_flag_.test(std::memory_order_acquire); }
bool F2fs::IsTearDown() const { return teardown_flag_.test(std::memory_order_relaxed); }
void F2fs::SetTearDown() { teardown_flag_.test_and_set(std::memory_order_relaxed); }

// We guarantee that this checkpoint procedure should not fail.
zx_status_t F2fs::WriteCheckpoint(bool blocked, bool is_umount) {
  if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  // Stop writeback during checkpoint.
  FlagAcquireGuard flag(&stop_reclaim_flag_);
  if (flag.IsAcquired()) {
    ZX_ASSERT(WaitForWriteback().is_ok());
  }
  ZX_DEBUG_ASSERT(segment_manager_->FreeSections() > GetFreeSectionsForDirtyPages());
  FlushDirsAndNodes();

  // update checkpoint pack index
  // Increase the version number so that
  // SIT entries and seg summaries are written at correct place
  superblock_info_->UpdateCheckpointVer();

  // write cached NAT/SIT entries to NAT/SIT area
  if (zx_status_t ret = GetNodeManager().FlushNatEntries(); ret != ZX_OK) {
    return ret;
  }
  if (zx_status_t ret = GetSegmentManager().FlushSitEntries(); ret != ZX_OK) {
    return ret;
  }

  // unlock all the fs_lock[] in do_checkpoint()
  if (zx_status_t ret = DoCheckpoint(is_umount); ret != ZX_OK) {
    return ret;
  }

  if (is_umount && !(superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag))) {
    ZX_ASSERT(superblock_info_->GetPageCount(CountType::kDirtyDents) == 0);
    ZX_ASSERT(superblock_info_->GetPageCount(CountType::kDirtyData) == 0);
    ZX_ASSERT(superblock_info_->GetPageCount(CountType::kWriteback) == 0);
    ZX_ASSERT(superblock_info_->GetPageCount(CountType::kDirtyMeta) == 0);
    ZX_ASSERT(superblock_info_->GetPageCount(CountType::kDirtyNodes) == 0);
  }
  return ZX_OK;
}

}  // namespace f2fs
