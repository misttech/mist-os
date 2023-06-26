// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

// Lock ordering for the change of data block address:
// ->data_page
//  ->node_page
//    update block addresses in the node page
void VnodeF2fs::SetDataBlkaddr(NodePage &node_page, uint32_t ofs_in_node, block_t new_addr) {
  node_page.WaitOnWriteback();
  if (new_addr == kNewAddr) {
    ZX_DEBUG_ASSERT(node_page.GetBlockAddr(ofs_in_node) == kNullAddr);
  } else {
    ZX_DEBUG_ASSERT(node_page.GetBlockAddr(ofs_in_node) != kNullAddr);
  }

  node_page.SetBlockAddr(ofs_in_node, new_addr);
  LockedPage lock_page(fbl::RefPtr<Page>(&node_page), false);
  lock_page.SetDirty();
  [[maybe_unused]] auto unused = lock_page.release(false);
}

zx_status_t VnodeF2fs::ReserveNewBlock(NodePage &node_page, uint32_t ofs_in_node) {
  if (TestFlag(InodeInfoFlag::kNoAlloc)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if (zx_status_t ret = fs()->IncValidBlockCount(this, 1); ret != ZX_OK) {
    return ret;
  }

  SetDataBlkaddr(node_page, ofs_in_node, kNewAddr);
  SetDirty();
  return ZX_OK;
}

#if 0  // porting needed
// int VnodeF2fs::CheckExtentCache(inode *inode, pgoff_t pgofs,
//           buffer_head *bh_result)
// {
//   Inode_info *fi = F2FS_I(inode);
//   SuperblockInfo *superblock_info = F2FS_SB(inode->i_sb);
//   pgoff_t start_fofs, end_fofs;
//   block_t start_blkaddr;

//   ReadLock(&fi->ext.ext_lock);
//   if (fi->ext.len == 0) {
//     ReadUnlock(&fi->ext.ext_lock);
//     return 0;
//   }

//   ++superblock_info->total_hit_ext;
//   start_fofs = fi->ext.fofs;
//   end_fofs = fi->ext.fofs + fi->ext.len - 1;
//   start_blkaddr = fi->ext.blk_addr;

//   if (pgofs >= start_fofs && pgofs <= end_fofs) {
//     uint32_t blkbits = inode->i_sb->s_blocksize_bits;
//     size_t count;

//     clear_buffer_new(bh_result);
//     map_bh(bh_result, inode->i_sb,
//        start_blkaddr + pgofs - start_fofs);
//     count = end_fofs - pgofs + 1;
//     if (count < (UINT_MAX >> blkbits))
//       bh_result->b_size = (count << blkbits);
//     else
//       bh_result->b_size = UINT_MAX;

//     ++superblock_info->read_hit_ext;
//     ReadUnlock(&fi->ext.ext_lock);
//     return 1;
//   }
//   ReadUnlock(&fi->ext.ext_lock);
//   return 0;
// }
#endif

void VnodeF2fs::UpdateExtentCache(block_t blk_addr, pgoff_t file_offset) {
#if 0  // TODO(fxbug.dev/118687): the extent cache has not been ported yet.
  pgoff_t start_fofs, end_fofs;
  block_t start_blkaddr, end_blkaddr;

  ZX_DEBUG_ASSERT(blk_addr != kNewAddr);
  do {
    std::lock_guard ext_lock(extent_cache_lock);

    start_fofs = extent_cache_.fofs;
    end_fofs = extent_cache_.fofs + extent_cache_.->ext.len - 1;
    start_blkaddr = extent_cache_blk_addr;
    end_blkaddr = extent_cache_.blk_addr + extent_cache_.len - 1;

    // Drop and initialize the matched extent
    if (extent_cache_.len == 1 && file_offset == start_fofs) {
      extent_cache_.len = 0;
		}

    // Initial extent
    if (extent_cache_.len == 0) {
      if (blk_addr != kNullAddr) {
        extent_cache_.fofs = file_offset;
        extent_cache_.blk_addr = blk_addr;
        extent_cache_.len = 1;
      }
      break;
    }

    // Frone merge
    if (file_offset == start_fofs - 1 && blk_addr == start_blkaddr - 1) {
      --extent_cache_.fofs;
      --extent_cache_.blk_addr;
      ++extent_cache_.len;
      break;
    }

    // Back merge
    if (file_offset == end_fofs + 1 && blk_addr == end_blkaddr + 1) {
      ++extent_cache_.len;
      break;
    }

    /* Split the existing extent */
    if (extent_cache_.len > 1 && file_offset >= start_fofs && file_offset <= end_fofs) {
      if ((end_fofs - file_offset) < (fi->ext.len >> 1)) {
        extent_cache_.len = static_cast<uint32_t>(file_offset - start_fofs);
      } else {
        extent_cache_.fofs = file_offset + 1;
        extent_cache_.blk_addr = static_cast<uint32_t>(start_blkaddr + file_offset - start_fofs + 1);
        extent_cache_.len -= file_offset - start_fofs + 1;
      }
      break;
    }
    return;
  } while (false);
#endif
  SetDirty();
}

zx::result<block_t> VnodeF2fs::FindDataBlkAddr(pgoff_t index) {
  uint32_t ofs_in_dnode;
  if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, index); result.is_error()) {
    return result.take_error();
  } else {
    ofs_in_dnode = result.value();
  }

  LockedPage dnode_page;
  if (zx_status_t err = fs()->GetNodeManager().FindLockedDnodePage(*this, index, &dnode_page);
      err != ZX_OK) {
    return zx::error(err);
  }

  return zx::ok(dnode_page.GetPage<NodePage>().GetBlockAddr(ofs_in_dnode));
}

zx::result<LockedPage> VnodeF2fs::FindDataPage(pgoff_t index, bool do_read) {
  fbl::RefPtr<Page> page;
  if (FindPage(index, &page) == ZX_OK && page->IsUptodate()) {
    LockedPage locked_page = LockedPage(std::move(page));
    return zx::ok(std::move(locked_page));
  }

  auto addr_or = FindDataBlkAddr(index);
  if (addr_or.is_error()) {
    return addr_or.take_error();
  }
  if (*addr_or == kNullAddr) {
    return zx::error(ZX_ERR_NOT_FOUND);
  } else if (*addr_or == kNewAddr) {
    // By fallocate(), there is no cached page, but with kNewAddr
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  LockedPage locked_page;
  if (zx_status_t err = GrabCachePage(index, &locked_page); err != ZX_OK) {
    return zx::error(err);
  }

  if (do_read) {
    if (auto status = fs()->MakeReadOperation(locked_page, *addr_or, PageType::kData);
        status.is_error()) {
      return status.take_error();
    }
  }

  return zx::ok(std::move(locked_page));
}

zx::result<LockedPagesAndAddrs> VnodeF2fs::FindDataBlockAddrsAndPages(const pgoff_t start,
                                                                      const pgoff_t end) {
  LockedPagesAndAddrs addrs_and_pages;
  ZX_DEBUG_ASSERT(end > start);
  size_t len = end - start;
  addrs_and_pages.block_addrs.resize(len, kNullAddr);
  addrs_and_pages.pages.resize(len);

  // If pages are in FileCache and up-to-date, we don't need to read that pages from the underlying
  // device.
  auto pages_or = FindPages(start, end);
  if (pages_or.is_error()) {
    return pages_or.take_error();
  }

  // Insert existing pages
  for (auto &page : pages_or.value()) {
    auto index = page->GetKey() - start;
    addrs_and_pages.pages[index] = std::move(page);
  }

  std::vector<pgoff_t> offsets;
  offsets.reserve(len);
  for (uint32_t index = 0; index < end - start; ++index) {
    if (!addrs_and_pages.pages[index] || !addrs_and_pages.pages[index]->IsUptodate()) {
      offsets.push_back(start + index);
    }
  }

  if (offsets.empty()) {
    return zx::ok(std::move(addrs_and_pages));
  }
  auto addrs_or = fs()->GetNodeManager().GetDataBlockAddresses(*this, offsets, true);
  if (addrs_or.is_error()) {
    return addrs_or.take_error();
  }
  ZX_DEBUG_ASSERT(offsets.size() == addrs_or.value().size());

  for (uint32_t i = 0; i < offsets.size(); ++i) {
    auto addrs_and_pages_index = offsets[i] - start;
    if (addrs_or.value()[i] != kNullAddr) {
      addrs_and_pages.block_addrs[addrs_and_pages_index] = addrs_or.value()[i];
      if (!addrs_and_pages.pages[addrs_and_pages_index]) {
        if (auto result = GrabCachePage(offsets[i], &addrs_and_pages.pages[addrs_and_pages_index]);
            result != ZX_OK) {
          return zx::error(result);
        }
      }
    }
  }

  return zx::ok(std::move(addrs_and_pages));
}

// If it tries to access a hole, return an error
// because the callers in dir.cc and gc.cc should be able to know
// whether this page exists or not.
zx_status_t VnodeF2fs::GetLockedDataPage(pgoff_t index, LockedPage *out) {
  auto page_or = GetLockedDataPages(index, index + 1);
  if (page_or.is_error()) {
    return page_or.error_value();
  }
  if (page_or->empty() || page_or.value()[0] == nullptr) {
    return ZX_ERR_NOT_FOUND;
  }

  *out = std::move(page_or.value()[0]);
  return ZX_OK;
}

zx::result<std::vector<LockedPage>> VnodeF2fs::GetLockedDataPages(const pgoff_t start,
                                                                  const pgoff_t end) {
  LockedPagesAndAddrs addrs_and_pages;
  if (auto addrs_and_pages_or = FindDataBlockAddrsAndPages(start, end);
      addrs_and_pages_or.is_error()) {
    return addrs_and_pages_or.take_error();
  } else {
    addrs_and_pages = std::move(addrs_and_pages_or.value());
  }

  if (addrs_and_pages.block_addrs.empty()) {
    return zx::ok(std::move(addrs_and_pages.pages));
  }

  bool need_read_op = false;
  for (uint32_t i = 0; i < addrs_and_pages.block_addrs.size(); ++i) {
    if (addrs_and_pages.block_addrs[i] != kNullAddr && !addrs_and_pages.pages[i]->IsUptodate()) {
      need_read_op = true;
      break;
    }
  }
  if (!need_read_op) {
    return zx::ok(std::move(addrs_and_pages.pages));
  }

  auto status =
      fs()->MakeReadOperations(addrs_and_pages.pages, addrs_and_pages.block_addrs, PageType::kData);
  if (status.is_error()) {
    return status.take_error();
  }

  return zx::ok(std::move(addrs_and_pages.pages));
}

// Caller ensures that this data page is never allocated.
// A new zero-filled data page is allocated in the page cache.
zx_status_t VnodeF2fs::GetNewDataPage(pgoff_t index, bool new_i_size, LockedPage *out) {
  block_t data_blkaddr;
  {
    LockedPage dnode_page;
    if (zx_status_t err = fs()->GetNodeManager().GetLockedDnodePage(*this, index, &dnode_page);
        err != ZX_OK) {
      return err;
    }

    uint32_t ofs_in_dnode;
    if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, index); result.is_error()) {
      return result.error_value();
    } else {
      ofs_in_dnode = result.value();
    }

    data_blkaddr = dnode_page.GetPage<NodePage>().GetBlockAddr(ofs_in_dnode);
    if (data_blkaddr == kNullAddr) {
      if (zx_status_t ret = ReserveNewBlock(dnode_page.GetPage<NodePage>(), ofs_in_dnode);
          ret != ZX_OK) {
        return ret;
      }
      data_blkaddr = kNewAddr;
    }
  }

  LockedPage page;
  if (zx_status_t ret = GrabCachePage(index, &page); ret != ZX_OK) {
    return ret;
  }

  if (page->IsUptodate()) {
    *out = std::move(page);
    return ZX_OK;
  }

  if (data_blkaddr == kNewAddr) {
    page->SetUptodate();
    page.Zero();
  } else {
    ZX_ASSERT_MSG(data_blkaddr == kNewAddr, "%lu page should have kNewAddr but (0x%x)",
                  page->GetKey(), data_blkaddr);
  }

  if (new_i_size && GetSize() < ((index + 1) << kPageCacheShift)) {
    SetSize((index + 1) << kPageCacheShift);
    // TODO: mark sync when fdatasync is available.
    SetFlag(InodeInfoFlag::kUpdateDir);
    SetDirty();
  }

  *out = std::move(page);
  return ZX_OK;
}

#if 0  // porting needed
/**
 * This function should be used by the data read flow only where it
 * does not check the "create" flag that indicates block allocation.
 * The reason for this special functionality is to exploit VFS readahead
 * mechanism.
 */
// int VnodeF2fs::GetDataBlockRo(inode *inode, sector_t iblock,
//       buffer_head *bh_result, int create)
// {
//   uint32_t blkbits = inode->i_sb->s_blocksize_bits;
//   unsigned maxblocks = bh_result.value().b_size > blkbits;
//   DnodeOfData dn;
//   pgoff_t pgofs;
//   //int err = 0;

//   /* Get the page offset from the block offset(iblock) */
//   pgofs =  (pgoff_t)(iblock >> (kPageCacheShift - blkbits));

//   if (VnodeF2fs::CheckExtentCache(inode, pgofs, bh_result))
//     return 0;

//   /* When reading holes, we need its node page */
//   //TODO(unknown): inode should be replaced with vnodef2fs
//   //SetNewDnode(&dn, inode, nullptr, nullptr, 0);
//   // TODO(unknown): should be replaced with NodeManager->GetDnodeOfData
//   /*err = get_DnodeOfData(&dn, pgofs, kRdOnlyNode);
//   if (err)
//     return (err == ZX_ERR_NOT_FOUND) ? 0 : err; */

//   /* It does not support data allocation */
//   ZX_ASSERT(!create);

//   if (dn.data_blkaddr != kNewAddr && dn.data_blkaddr != kNullAddr) {
//     uint32_t end_offset;

//     end_offset = IsInode(dn.node_page) ?
//         kAddrsPerInode :
//         kAddrsPerBlock;

//     clear_buffer_new(bh_result);

//     /* Give more consecutive addresses for the read ahead */
//     for (uint32_t i = 0; i < end_offset - dn.ofs_in_node; ++i)
//       if (((DatablockAddr(dn.node_page,
//               dn.ofs_in_node + i))
//         != (dn.data_blkaddr + i)) || maxblocks == i)
//         break;
//     //map_bh(bh_result, inode->i_sb, dn.data_blkaddr);
//     bh_result->b_size = (i << blkbits);
//   }
//   F2fsPutDnode(&dn);
//   return 0;
// }
#endif

zx::result<block_t> VnodeF2fs::GetBlockAddrForDataPage(LockedPage &page) {
  LockedPage dnode_page;
  if (zx_status_t err =
          fs()->GetNodeManager().FindLockedDnodePage(*this, page->GetIndex(), &dnode_page);
      err != ZX_OK) {
    return zx::error(err);
  }

  uint32_t ofs_in_dnode;
  if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, page->GetIndex());
      result.is_error()) {
    return result.take_error();
  } else {
    ofs_in_dnode = result.value();
  }

  block_t old_blk_addr = dnode_page.GetPage<NodePage>().GetBlockAddr(ofs_in_dnode);
  // This page is already truncated
  if (old_blk_addr == kNullAddr) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  block_t new_blk_addr = kNullAddr;
  if (old_blk_addr != kNewAddr && !page->IsColdData() &&
      fs()->GetSegmentManager().NeedInplaceUpdate(this)) {
    new_blk_addr = old_blk_addr;
  } else {
    pgoff_t file_offset = page->GetIndex();
    auto addr_or = fs()->GetSegmentManager().GetBlockAddrForDataPage(
        page, dnode_page.GetPage<NodePage>().NidOfNode(), ofs_in_dnode, old_blk_addr);
    ZX_ASSERT(addr_or.is_ok());
    new_blk_addr = *addr_or;
    SetDataBlkaddr(dnode_page.GetPage<NodePage>(), ofs_in_dnode, new_blk_addr);
    UpdateExtentCache(new_blk_addr, file_offset);
    UpdateVersion(LeToCpu(fs()->GetSuperblockInfo().GetCheckpoint().checkpoint_ver));
  }

  return zx::ok(new_blk_addr);
}

zx::result<block_t> VnodeF2fs::GetBlockAddrForDirtyDataPage(LockedPage &page, bool is_reclaim) {
  const pgoff_t end_index = (GetSize() >> kPageCacheShift);
  block_t blk_addr = kNullAddr;

  if (page->GetIndex() >= end_index) {
    unsigned offset = GetSize() & (kPageSize - 1);
    if ((page->GetIndex() >= end_index + 1) || !offset) {
      // This page is already truncated
      page->ClearDirtyForIo();
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    // The lock and writeback flags prevent its data to be changed or truncated during this
    // writeback.
  }

  // TODO: Consider skipping the wb for hot/warm blocks
  // since a higher temp. block has more chances to be updated sooner.
  // if (superblock_info.IsOnRecovery()) {
  // TODO: Tracks pages skipping wb
  // ++wbc->pages_skipped;
  // page->SetDirty();
  // return kAopWritepageActivate;
  //}

  if (page->ClearDirtyForIo()) {
    page->SetWriteback();
    auto addr_or = GetBlockAddrForDataPage(page);
    if (addr_or.is_error()) {
      // TODO: Tracks pages skipping wb
      // ++wbc->pages_skipped;
      return addr_or.take_error();
    }
    blk_addr = *addr_or;
  }
  return zx::ok(blk_addr);
}

zx::result<std::vector<LockedPage>> VnodeF2fs::WriteBegin(const size_t offset, const size_t len) {
  const pgoff_t index_start = safemath::CheckDiv<pgoff_t>(offset, kBlockSize).ValueOrDie();
  const size_t offset_end = safemath::CheckAdd<size_t>(offset, len).ValueOrDie();
  const pgoff_t index_end = CheckedDivRoundUp<pgoff_t>(offset_end, kBlockSize);

  fs::SharedLock rlock(fs()->GetSuperblockInfo().GetFsLock(LockType::kFileOp));
  std::vector<LockedPage> data_pages;
  data_pages.reserve(index_end - index_start);
  auto pages_or = GrabCachePages(index_start, index_end);
  if (unlikely(pages_or.is_error())) {
    return pages_or.take_error();
  }
  // If |this| is an orphan, we don't need to set dirty flag for |*pages_or|.
  if (file_cache_->IsOrphan()) {
    ZX_DEBUG_ASSERT(!HasLink());
    return zx::ok(std::move(pages_or.value()));
  }
  data_pages = std::move(pages_or.value());
  for (auto &page : data_pages) {
    page->WaitOnWriteback();
    page.SetDirty();
  }
  std::vector<block_t> data_block_addresses;
  if (auto result =
          fs()->GetNodeManager().GetDataBlockAddresses(*this, index_start, index_end - index_start);
      result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::move(data_pages));
}

}  // namespace f2fs
