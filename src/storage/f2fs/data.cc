// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <numeric>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

zx_status_t VnodeF2fs::ReserveNewBlock(NodePage &node_page, size_t ofs_in_node) {
  if (TestFlag(InodeInfoFlag::kNoAlloc)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if (zx_status_t ret = superblock_info_.IncValidBlockCount(1); ret != ZX_OK) {
    if (ret == ZX_ERR_NO_SPACE) {
      fs()->GetInspectTree().OnOutOfSpace();
    }
    return ret;
  }

  IncBlocks(1);
  node_page.SetDataBlkaddr(ofs_in_node, kNewAddr);
  SetDirty();
  return ZX_OK;
}

void VnodeF2fs::UpdateExtentCache(pgoff_t file_offset, block_t blk_addr, uint32_t len) {
  if (!ExtentCacheAvailable()) {
    return;
  }

  if (auto result = extent_tree_->InsertExtent(ExtentInfo{file_offset, blk_addr, len});
      result.is_error()) {
    SetFlag(InodeInfoFlag::kNoExtent);
    return;
  }

  SetDirty();
}

zx::result<block_t> VnodeF2fs::LookupExtentCacheBlock(pgoff_t file_offset) {
  if (!ExtentCacheAvailable()) {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }

  auto extent_info = extent_tree_->LookupExtent(file_offset);
  if (extent_info.is_error()) {
    return extent_info.take_error();
  }

  return zx::ok(extent_info->blk_addr +
                safemath::checked_cast<uint32_t>(file_offset - extent_info->fofs));
}

zx::result<block_t> VnodeF2fs::FindDataBlkAddr(pgoff_t index) {
  if (auto data_blkaddr_or = LookupExtentCacheBlock(index); data_blkaddr_or.is_ok()) {
    ZX_DEBUG_ASSERT(data_blkaddr_or.value() != kNullAddr || data_blkaddr_or.value() != kNewAddr);
    return zx::ok(data_blkaddr_or.value());
  }

  auto path_or = GetNodePath(*this, index);
  if (path_or.is_error()) {
    return path_or.take_error();
  }
  size_t ofs_in_dnode = GetOfsInDnode(*path_or);
  auto dnode_page_or = fs()->GetNodeManager().FindLockedDnodePage(*path_or);
  if (dnode_page_or.is_error()) {
    return dnode_page_or.take_error();
  }
  return zx::ok((*dnode_page_or).GetPage<NodePage>().GetBlockAddr(ofs_in_dnode));
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
  auto addrs_or = GetDataBlockAddresses(offsets, true);
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
    if (addrs_and_pages.block_addrs[i] == kNewAddr) {
      addrs_and_pages.pages[i]->SetUptodate();
    } else if (addrs_and_pages.block_addrs[i] != kNullAddr &&
               !addrs_and_pages.pages[i]->IsUptodate()) {
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
    auto path_or = GetNodePath(*this, index);
    if (path_or.is_error()) {
      return path_or.status_value();
    }
    auto page_or = fs()->GetNodeManager().GetLockedDnodePage(*path_or, IsDir());
    if (page_or.is_error()) {
      return page_or.error_value();
    }
    IncBlocks(path_or->num_new_nodes);
    LockedPage dnode_page = std::move(*page_or);
    size_t ofs_in_dnode = GetOfsInDnode(*path_or);
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

  size_t new_size = (index + 1) * kPageSize;
  if (new_i_size && GetSize() < new_size) {
    SetSize(new_size);
    SetFlag(InodeInfoFlag::kUpdateDir);
    SetFlag(InodeInfoFlag::kSyncInode);
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

block_t VnodeF2fs::GetBlockAddr(LockedPage &page) {
  ZX_DEBUG_ASSERT(page->IsUptodate());
  if (!page->ClearDirtyForIo()) {
    return kNullAddr;
  }
  if (IsMeta()) {
    block_t addr = safemath::checked_cast<block_t>(page->GetIndex());
    ZX_ASSERT(page->SetBlockAddr(addr).is_ok());
    return addr;
  }
  ZX_DEBUG_ASSERT(IsNode());
  nid_t nid = page.GetPage<NodePage>().NidOfNode();
  ZX_DEBUG_ASSERT(page->GetIndex() == nid);

  NodeInfo ni;
  fs_->GetNodeManager().GetNodeInfo(nid, ni);
  block_t old_addr = ni.blk_addr;

  // This page is already truncated
  if (old_addr == kNullAddr) {
    return kNullAddr;
  }

  Summary sum;
  SetSummary(&sum, nid, 0, ni.version);
  block_t new_addr =
      fs_->GetSegmentManager().GetBlockAddrOnSegment(page, old_addr, &sum, PageType::kNode);
  ZX_DEBUG_ASSERT(new_addr != kNullAddr && new_addr != kNewAddr && new_addr != old_addr);

  fs_->GetNodeManager().SetNodeAddr(ni, new_addr);
  ZX_ASSERT(page->SetBlockAddr(new_addr).is_ok());
  return new_addr;
}

block_t VnodeF2fs::GetBlockAddrOnDataSegment(LockedPage &page) {
  ZX_DEBUG_ASSERT(page->IsUptodate());
  ZX_DEBUG_ASSERT(!IsMeta());
  ZX_DEBUG_ASSERT(!IsNode());
  if (!page->ClearDirtyForIo()) {
    return kNullAddr;
  }
  const pgoff_t end_index = GetSize() / kPageSize;
  if (page->GetIndex() >= end_index) {
    unsigned offset = GetSize() & (kPageSize - 1);
    if ((page->GetIndex() >= end_index + 1) || !offset) {
      return kNullAddr;
    }
  }
  auto path_or = GetNodePath(*this, page->GetIndex());
  if (path_or.is_error()) {
    return kNullAddr;
  }

  auto dnode_page_or = fs()->GetNodeManager().FindLockedDnodePage(*path_or);
  if (dnode_page_or.is_error()) {
    if (page->IsUptodate() && dnode_page_or.status_value() != ZX_ERR_NOT_FOUND) {
      // In case of failure, we just redirty it.
      page.SetDirty();
      FX_LOGS(WARNING) << "failed to allocate a block." << dnode_page_or.status_string();
    }
    return kNullAddr;
  }

  size_t ofs_in_dnode = GetOfsInDnode(*path_or);
  block_t old_addr = (*dnode_page_or).GetPage<NodePage>().GetBlockAddr(ofs_in_dnode);
  // This page is already truncated
  if (old_addr == kNullAddr) {
    return kNullAddr;
  }
  // Check if IPU is allowed
  if (old_addr != kNewAddr && !page->IsColdData() &&
      fs_->GetSegmentManager().NeedInplaceUpdate(IsDir())) {
    return old_addr;
  }

  // Allocate a new addr
  NodeInfo ni;
  nid_t nid = (*dnode_page_or).GetPage<NodePage>().NidOfNode();
  fs_->GetNodeManager().GetNodeInfo(nid, ni);

  Summary sum;
  SetSummary(&sum, nid, ofs_in_dnode, ni.version);
  block_t new_addr =
      fs_->GetSegmentManager().GetBlockAddrOnSegment(page, old_addr, &sum, PageType::kData);
  ZX_DEBUG_ASSERT(new_addr != kNullAddr && new_addr != kNewAddr && new_addr != old_addr);

  (*dnode_page_or).GetPage<NodePage>().SetDataBlkaddr(ofs_in_dnode, new_addr);
  UpdateExtentCache(page->GetIndex(), new_addr);
  UpdateVersion(superblock_info_.GetCheckpointVer());
  ZX_ASSERT(page->SetBlockAddr(new_addr).is_ok());
  return new_addr;
}

zx::result<std::vector<LockedPage>> VnodeF2fs::WriteBegin(const size_t offset, const size_t len) {
  const pgoff_t index_start = safemath::CheckDiv<pgoff_t>(offset, kBlockSize).ValueOrDie();
  const size_t offset_end = safemath::CheckAdd<size_t>(offset, len).ValueOrDie();
  const pgoff_t index_end = CheckedDivRoundUp<pgoff_t>(offset_end, kBlockSize);

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
  if (auto result = GetDataBlockAddresses(index_start, index_end - index_start);
      result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::move(data_pages));
}

zx::result<std::vector<block_t>> VnodeF2fs::GetDataBlockAddresses(
    const std::vector<pgoff_t> &indices, bool read_only) {
  std::vector<block_t> data_block_addresses(indices.size());
  uint32_t prev_node_offset = kInvalidNodeOffset;
  LockedPage dnode_page;

  for (uint32_t iter = 0; iter < indices.size(); ++iter) {
    if (auto data_blkaddr_or = LookupExtentCacheBlock(indices[iter]); data_blkaddr_or.is_ok()) {
      ZX_DEBUG_ASSERT(data_blkaddr_or.value() != kNullAddr || data_blkaddr_or.value() != kNewAddr);
      data_block_addresses[iter] = data_blkaddr_or.value();
      continue;
    }

    auto path_or = GetNodePath(*this, indices[iter]);
    if (path_or.is_error()) {
      return path_or.take_error();
    }
    if (!IsSameDnode(*path_or, prev_node_offset)) {
      dnode_page.reset();
      if (read_only) {
        auto dnode_page_or = fs()->GetNodeManager().FindLockedDnodePage(*path_or);
        if (dnode_page_or.is_error()) {
          if (dnode_page_or.error_value() == ZX_ERR_NOT_FOUND) {
            prev_node_offset = kInvalidNodeOffset;
            data_block_addresses[iter] = kNullAddr;
            continue;
          }
          return dnode_page_or.take_error();
        }
        dnode_page = std::move(*dnode_page_or);
      } else {
        auto dnode_page_or = fs()->GetNodeManager().GetLockedDnodePage(*path_or, IsDir());
        if (dnode_page_or.is_error()) {
          return dnode_page_or.take_error();
        }
        IncBlocks(path_or->num_new_nodes);
        dnode_page = std::move(*dnode_page_or);
      }
      prev_node_offset = dnode_page.GetPage<NodePage>().OfsOfNode();
    }
    ZX_DEBUG_ASSERT(dnode_page != nullptr);

    size_t ofs_in_dnode = GetOfsInDnode(*path_or);
    block_t data_blkaddr = dnode_page.GetPage<NodePage>().GetBlockAddr(ofs_in_dnode);

    if (!read_only && data_blkaddr == kNullAddr) {
      if (zx_status_t err = ReserveNewBlock(dnode_page.GetPage<NodePage>(), ofs_in_dnode);
          err != ZX_OK) {
        return zx::error(err);
      }
      data_blkaddr = kNewAddr;
    }

    data_block_addresses[iter] = data_blkaddr;
  }
  return zx::ok(std::move(data_block_addresses));
}

zx::result<std::vector<block_t>> VnodeF2fs::GetDataBlockAddresses(pgoff_t index, size_t count,
                                                                  bool read_only) {
  std::vector<pgoff_t> indices(count);
  std::iota(indices.begin(), indices.end(), index);
  return GetDataBlockAddresses(indices, read_only);
}

}  // namespace f2fs
