// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/bcache.h"
#include "src/storage/f2fs/dir.h"
#include "src/storage/f2fs/f2fs.h"
#include "src/storage/f2fs/node.h"
#include "src/storage/f2fs/node_page.h"
#include "src/storage/f2fs/segment.h"
#include "src/storage/f2fs/vnode.h"

namespace f2fs {

F2fs::FsyncInodeEntry *F2fs::GetFsyncInode(FsyncInodeList &inode_list, nid_t ino) {
  auto inode_entry =
      inode_list.find_if([ino](const auto &entry) { return entry.GetVnode().Ino() == ino; });
  if (inode_entry == inode_list.end())
    return nullptr;
  return &(*inode_entry);
}

zx_status_t F2fs::RecoverDentry(NodePage &ipage, VnodeF2fs &vnode) {
  if (!ipage.IsDentDnode()) {
    return ZX_OK;
  }

  auto vnode_or = GetVnode(LeToCpu(ipage.GetAddress<Node>()->i.i_pino));
  if (vnode_or.is_error()) {
    return vnode_or.status_value();
  }
  return fbl::RefPtr<Dir>::Downcast(*vnode_or)->RecoverLink(vnode).status_value();
}

zx_status_t F2fs::RecoverInode(VnodeF2fs &vnode, NodePage &node_page) {
  Inode &inode = node_page.GetAddress<Node>()->i;

  vnode.SetMode(LeToCpu(inode.i_mode));
  vnode.SetTime<Timestamps::AccessTime>({static_cast<time_t>(LeToCpu(inode.i_atime)),
                                         static_cast<time_t>(LeToCpu(inode.i_atime_nsec))});
  vnode.SetTime<Timestamps::BirthTime>({static_cast<time_t>(LeToCpu(inode.i_ctime)),
                                        static_cast<time_t>(LeToCpu(inode.i_ctime_nsec))});
  vnode.SetTime<Timestamps::ModificationTime>({static_cast<time_t>(LeToCpu(inode.i_mtime)),
                                               static_cast<time_t>(LeToCpu(inode.i_mtime_nsec))});
  return RecoverDentry(node_page, vnode);
}

zx::result<F2fs::FsyncInodeList> F2fs::FindFsyncDnodes() {
  CursegInfo *curseg = segment_manager_->CURSEG_I(CursegType::kCursegWarmNode);
  // Get blkaddr from which it starts recovery
  block_t blkaddr = segment_manager_->StartBlock(curseg->segno) + curseg->next_blkoff;
  PageList inode_pages;
  FsyncInodeList inode_list;
  fs::SharedLock lock(f2fs::GetGlobalLock());

  while (true) {
    bool new_entry = false;
    LockedPage page;
    // We cannot get fsync node pages from GetNodePage which can retrieve only checkpointed node
    // pages. Instead, GetMetaPage is used to read blocks on non-checkpointed warm nodes segment and
    // get the corresponding pages.
    if (zx_status_t ret = GetMetaPage(blkaddr, &page); ret != ZX_OK) {
      return zx::error(ret);
    }

    if (superblock_info_->GetCheckpointVer(true) != page.GetPage<NodePage>().CpverOfNode()) {
      break;
    }

    auto node_page = fbl::RefPtr<NodePage>::Downcast(page.CopyRefPtr());
    if (!node_page->IsFsyncDnode()) {
      blkaddr = page.GetPage<NodePage>().NextBlkaddrOfNode();
      page->ClearUptodate();
      if (node_page->IsInode() && node_page->IsDentDnode()) {
        inode_pages.push_back(std::move(node_page));
      }
      continue;
    }

    fbl::RefPtr<NodePage> inode_page;
    if (node_page->IsInode()) {
      inode_page = node_page;
    }
    ino_t ino = node_page->InoOfNode();
    auto entry_ptr = GetFsyncInode(inode_list, ino);
    if (entry_ptr) {
      entry_ptr->SetLastDnodeBlkaddr(blkaddr);
      if (inode_page && inode_page->IsDentDnode()) {
        entry_ptr->GetVnode().SetFlag(InodeInfoFlag::kIncLink);
      }
    } else {
      if (!inode_page) {
        for (auto &tmp : inode_pages) {
          auto inode = fbl::RefPtr<NodePage>::Downcast(fbl::RefPtr<Page>(&tmp));
          if (inode->NidOfNode() == ino) {
            inode_page = inode;
          }
        }
      }
      if (inode_page && inode_page->IsDentDnode()) {
        if (zx_status_t err = GetNodeManager().RecoverInodePage(*inode_page); err != ZX_OK) {
          return zx::error(err);
        }
      }

      auto vnode_or = GetVnode(ino);
      if (vnode_or.is_error()) {
        return vnode_or.take_error();
      }

      fbl::RefPtr<VnodeF2fs> vnode_refptr = *std::move(vnode_or);
      auto entry = std::make_unique<FsyncInodeEntry>(std::move(vnode_refptr));
      entry->SetLastDnodeBlkaddr(blkaddr);
      inode_list.push_back(std::move(entry));
      entry_ptr = GetFsyncInode(inode_list, ino);
      new_entry = true;
    }
    if (inode_page) {
      ZX_DEBUG_ASSERT(inode_page->InoOfNode() == page.GetPage<NodePage>().InoOfNode());
      if (zx_status_t err = RecoverInode(entry_ptr->GetVnode(), *inode_page); err != ZX_OK) {
        return zx::error(err);
      }
      entry_ptr->SetSize(LeToCpu(inode_page->GetAddress<Node>()->i.i_size));
    } else if (new_entry) {
      LockedPage ipage;
      if (zx_status_t err = GetNodeVnode().GrabLockedPage(ino, &ipage); err != ZX_OK) {
        return zx::error(err);
      }
      entry_ptr->SetSize(LeToCpu(ipage->GetAddress<Node>()->i.i_size));
    }

    // Get the next block addr
    blkaddr = page.GetPage<NodePage>().NextBlkaddrOfNode();
    page->ClearUptodate();
  }
  if (inode_list.is_empty()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return zx::ok(std::move(inode_list));
}

void F2fs::CheckIndexInPrevNodes(block_t blkaddr) {
  uint32_t segno = segment_manager_->GetSegmentNumber(blkaddr);
  size_t blkoff =
      segment_manager_->GetSegOffFromSeg0(blkaddr) & (superblock_info_->GetBlocksPerSeg() - 1);
  Summary sum;
  const SegmentEntry &sentry = GetSegmentManager().GetSegmentEntry(segno);
  if (!sentry.cur_valid_map.GetOne(ToMsbFirst(blkoff))) {
    return;
  }

  // Get the previous summary
  int i;
  for (i = static_cast<int>(CursegType::kCursegWarmData);
       i <= static_cast<int>(CursegType::kCursegColdData); ++i) {
    CursegInfo *curseg = segment_manager_->CURSEG_I(static_cast<CursegType>(i));
    if (curseg->segno == segno) {
      sum = curseg->sum_blk->entries[blkoff];
      break;
    }
  }
  if (i > static_cast<int>(CursegType::kCursegColdData)) {
    LockedPage sum_page;
    GetSegmentManager().GetSumPage(segno, &sum_page);
    SummaryBlock *sum_node;
    sum_node = sum_page->GetAddress<SummaryBlock>();
    sum = sum_node->entries[blkoff];
  }

  fbl::RefPtr<VnodeF2fs> vnode_refptr;
  size_t bidx;
  // Get the node page
  {
    LockedPage node_page;
    if (zx_status_t err = GetNodeManager().GetNodePage(LeToCpu(sum.nid), &node_page);
        err != ZX_OK) {
      FX_LOGS(ERROR) << "F2fs::CheckIndexInPrevNodes, GetNodePage Error!!!";
      return;
    }
    nid_t ino = node_page.GetPage<NodePage>().InoOfNode();
    auto vnode_or = GetVnode(ino);
    ZX_ASSERT(vnode_or.is_ok());
    vnode_refptr = *std::move(vnode_or);
    bidx = node_page.GetPage<NodePage>().StartBidxOfNode(vnode_refptr->GetAddrsPerInode()) +
           LeToCpu(sum.ofs_in_node);
  }

  // Deallocate previous index in the node page
  vnode_refptr->TruncateHole(bidx, bidx + 1, true);
}

void F2fs::DoRecoverData(VnodeF2fs &vnode, NodePage &page) {
  size_t start, end;
  Summary sum;
  NodeInfo ni;

  if (vnode.RecoverInlineData(page) == ZX_OK) {
    return;
  }

  start = page.StartBidxOfNode(vnode.GetAddrsPerInode());
  if (page.IsInode()) {
    end = start + vnode.GetAddrsPerInode();
  } else {
    end = start + kAddrsPerBlock;
  }

  auto path_or = vnode.GetNodePath(start);
  if (path_or.is_error()) {
    return;
  }
  auto page_or = GetNodeManager().GetLockedDnodePage(*path_or, vnode.IsDir());
  if (page_or.is_error()) {
    return;
  }
  vnode.IncBlocks(path_or->num_new_nodes);
  LockedPage dnode_page = std::move(*page_or);
  dnode_page.WaitOnWriteback();

  GetNodeManager().GetNodeInfo(dnode_page.GetPage<NodePage>().NidOfNode(), ni);
  ZX_DEBUG_ASSERT(ni.ino == page.InoOfNode());
  ZX_DEBUG_ASSERT(dnode_page.GetPage<NodePage>().OfsOfNode() == page.OfsOfNode());

  size_t offset_in_dnode = GetOfsInDnode(*path_or);

  for (; start < end; ++start) {
    block_t src, dest;

    src = dnode_page.GetPage<NodePage>().GetBlockAddr(offset_in_dnode);
    dest = page.GetBlockAddr(offset_in_dnode);

    if (src != dest && dest != kNewAddr && dest != kNullAddr) {
      if (src == kNullAddr) {
        ZX_ASSERT(vnode.ReserveNewBlock(dnode_page, offset_in_dnode) == ZX_OK);
      }

      // Check the previous node page having this index
      CheckIndexInPrevNodes(dest);

      SetSummary(&sum, dnode_page.GetPage<NodePage>().NidOfNode(), offset_in_dnode, ni.version);

      // Write dummy data page
      GetSegmentManager().RecoverDataPage(sum, src, dest);
      dnode_page.WaitOnWriteback();
      dnode_page.GetPage<NodePage>().SetDataBlkaddr(offset_in_dnode, dest);
      dnode_page.SetDirty();
      vnode.UpdateExtentCache(page.StartBidxOfNode(vnode.GetAddrsPerInode()) + offset_in_dnode,
                              dest);
    }
    ++offset_in_dnode;
  }

  dnode_page.GetPage<NodePage>().CopyNodeFooterFrom(page);
  dnode_page.GetPage<NodePage>().FillNodeFooter(ni.nid, ni.ino, page.OfsOfNode());
  dnode_page.SetDirty();
}

void F2fs::RecoverData(FsyncInodeList &inode_list) {
  block_t blkaddr = segment_manager_->NextFreeBlkAddr(CursegType::kCursegWarmNode);

  while (true) {
    LockedPage page;
    // Eliminate duplicate node block reads using a meta inode cache.
    if (zx_status_t ret = GetMetaVnode().GrabLockedPage(blkaddr, &page); ret != ZX_OK) {
      return;
    }

    auto status = MakeReadOperation(page, blkaddr, PageType::kNode);
    if (status.is_error()) {
      break;
    }

    if (superblock_info_->GetCheckpointVer(true) != page.GetPage<NodePage>().CpverOfNode()) {
      break;
    }

    if (auto entry = GetFsyncInode(inode_list, page.GetPage<NodePage>().InoOfNode());
        entry != nullptr) {
      DoRecoverData(entry->GetVnode(), page.GetPage<NodePage>());
      if (entry->GetLastDnodeBlkaddr() == blkaddr) {
        inode_list.erase(*entry);
      }
    }
    // check next segment
    blkaddr = page.GetPage<NodePage>().NextBlkaddrOfNode();
    page->ClearUptodate();
  }

  GetSegmentManager().AllocateNewSegments();
}

void F2fs::RecoverFsyncData() {
  // Step #1: find fsynced inode numbers
  SetOnRecovery();
  if (auto result = FindFsyncDnodes(); result.is_ok()) {
    FsyncInodeList inode_list = std::move(*result);
    // Step #2: recover data
    for (auto &entry : inode_list) {
      auto &vnode = entry.GetVnode();
      ZX_ASSERT(vnode.InitFileCache(entry.GetSize()) == ZX_OK);
      vnode.SetDirty();
    }
    RecoverData(inode_list);
    ZX_DEBUG_ASSERT(inode_list.is_empty());
    GetMetaVnode().InvalidatePages(GetSegmentManager().GetMainAreaStartBlock());
    SyncFs(false);
  }
  GetMetaVnode().InvalidatePages(GetSegmentManager().GetMainAreaStartBlock());
  ClearOnRecovery();
}

}  // namespace f2fs
