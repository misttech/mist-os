// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

uint8_t *Dir::InlineDentryBitmap(Page *page) {
  Inode &inode = page->GetAddress<Node>()->i;
  return reinterpret_cast<uint8_t *>(
      &inode.i_addr[GetExtraISize() / sizeof(uint32_t) + kInlineStartOffset]);
}

uint64_t Dir::InlineDentryBitmapSize() const {
  return (MaxInlineDentry() + kBitsPerByte - 1) / kBitsPerByte;
}

DirEntry *Dir::InlineDentryArray(Page *page, VnodeF2fs &vnode) {
  uint8_t *base = InlineDentryBitmap(page);
  uint32_t reserved = safemath::checked_cast<uint32_t>(
      (vnode.MaxInlineData() -
       safemath::CheckMul(vnode.MaxInlineDentry(), (kSizeOfDirEntry + kDentrySlotLen)))
          .ValueOrDie());

  return reinterpret_cast<DirEntry *>(base + reserved);
}

uint8_t (*Dir::InlineDentryFilenameArray(Page *page, VnodeF2fs &vnode))[kDentrySlotLen] {
  uint8_t *base = InlineDentryBitmap(page);
  uint32_t reserved = safemath::checked_cast<uint32_t>(
      (vnode.MaxInlineData() - safemath::CheckMul(vnode.MaxInlineDentry(), kDentrySlotLen))
          .ValueOrDie());
  return reinterpret_cast<uint8_t(*)[kDentrySlotLen]>(base + reserved);
}

DirEntry *Dir::FindInInlineDir(std::string_view name, fbl::RefPtr<Page> *res_page) {
  LockedPage ipage;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(Ino(), &ipage); ret != ZX_OK)
    return nullptr;
  f2fs_hash_t namehash = DentryHash(name);

  for (uint32_t bit_pos = 0; bit_pos < MaxInlineDentry();) {
    bit_pos = FindNextBit(InlineDentryBitmap(ipage.get()), MaxInlineDentry(), bit_pos);
    if (bit_pos >= MaxInlineDentry()) {
      break;
    }

    DirEntry *de = &InlineDentryArray(ipage.get(), *this)[bit_pos];
    if (EarlyMatchName(name, namehash, *de)) {
      if (!memcmp(InlineDentryFilenameArray(ipage.get(), *this)[bit_pos], name.data(),
                  name.length())) {
        *res_page = ipage.release();

        if (de != nullptr) {
          fs()->GetDirEntryCache().UpdateDirEntry(Ino(), name, *de, kCachedInlineDirEntryPageIndex);
        }
        return de;
      }
    }

    // For the most part, it should be a bug when name_len is zero.
    // We stop here for figuring out where the bugs are occurred.
    ZX_DEBUG_ASSERT(de->name_len > 0);

    bit_pos += GetDentrySlots(LeToCpu(de->name_len));
  }

  return nullptr;
}

DirEntry *Dir::ParentInlineDir(fbl::RefPtr<Page> *out) {
  LockedPage ipage;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(Ino(), &ipage); ret != ZX_OK) {
    return nullptr;
  }

  DirEntry *de = &InlineDentryArray(ipage.get(), *this)[1];
  *out = ipage.release();
  return de;
}

zx_status_t Dir::MakeEmptyInlineDir(VnodeF2fs *vnode) {
  LockedPage ipage;

  if (zx_status_t err = fs()->GetNodeManager().GetNodePage(vnode->Ino(), &ipage); err != ZX_OK)
    return err;

  DirEntry *de = &InlineDentryArray(&(*ipage), *vnode)[0];
  de->name_len = CpuToLe(static_cast<uint16_t>(1));
  de->hash_code = 0;
  de->ino = CpuToLe(vnode->Ino());
  std::memcpy(InlineDentryFilenameArray(&(*ipage), *vnode)[0], ".", 1);
  SetDeType(de, vnode);

  de = &InlineDentryArray(&(*ipage), *vnode)[1];
  de->hash_code = 0;
  de->name_len = CpuToLe(static_cast<uint16_t>(2));
  de->ino = CpuToLe(Ino());
  std::memcpy(InlineDentryFilenameArray(&(*ipage), *vnode)[1], "..", 2);
  SetDeType(de, vnode);

  TestAndSetBit(0, InlineDentryBitmap(&(*ipage)));
  TestAndSetBit(1, InlineDentryBitmap(&(*ipage)));

  ipage.SetDirty();

  if (vnode->GetSize() < vnode->MaxInlineData()) {
    vnode->SetSize(vnode->MaxInlineData());
    vnode->SetFlag(InodeInfoFlag::kUpdateDir);
  }

  return ZX_OK;
}

unsigned int Dir::RoomInInlineDir(Page *ipage, uint32_t slots) {
  uint32_t bit_start = 0;

  while (true) {
    uint32_t zero_start = FindNextZeroBit(InlineDentryBitmap(ipage), MaxInlineDentry(), bit_start);
    if (zero_start >= MaxInlineDentry())
      return MaxInlineDentry();

    uint32_t zero_end = FindNextBit(InlineDentryBitmap(ipage), MaxInlineDentry(), zero_start);
    if (zero_end - zero_start >= slots)
      return zero_start;

    bit_start = zero_end + 1;

    if (zero_end + 1 >= MaxInlineDentry()) {
      return MaxInlineDentry();
    }
  }
}

zx_status_t Dir::ConvertInlineDir() {
  LockedPage page;
  if (zx_status_t ret = GrabCachePage(0, &page); ret != ZX_OK) {
    return ret;
  }

  LockedPage dnode_page;
  if (zx_status_t err = fs()->GetNodeManager().GetLockedDnodePage(*this, 0, &dnode_page);
      err != ZX_OK) {
    return err;
  }

  uint32_t ofs_in_dnode;
  if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, 0); result.is_error()) {
    return result.error_value();
  } else {
    ofs_in_dnode = result.value();
  }

  NodePage *ipage = &dnode_page.GetPage<NodePage>();
  block_t data_blkaddr = ipage->GetBlockAddr(ofs_in_dnode);
  ZX_DEBUG_ASSERT(data_blkaddr == kNullAddr);

  if (zx_status_t err = ReserveNewBlock(*ipage, ofs_in_dnode); err != ZX_OK) {
    return err;
  }

  page->WaitOnWriteback();
  page.Zero();

  DentryBlock *dentry_blk = page->GetAddress<DentryBlock>();

  // copy data from inline dentry block to new dentry block
  std::memcpy(dentry_blk->dentry_bitmap, InlineDentryBitmap(ipage), InlineDentryBitmapSize());
  std::memcpy(dentry_blk->dentry, InlineDentryArray(ipage, *this),
              sizeof(DirEntry) * MaxInlineDentry());
  std::memcpy(
      dentry_blk->filename, InlineDentryFilenameArray(ipage, *this),
      safemath::CheckMul(safemath::checked_cast<size_t>(MaxInlineDentry()), kNameLen).ValueOrDie());

  page.SetDirty();
  // clear inline dir and flag after data writeback
  dnode_page->WaitOnWriteback();
  dnode_page.Zero(InlineDataOffset(), InlineDataOffset() + MaxInlineData());
  ClearFlag(InodeInfoFlag::kInlineDentry);

  if (!TestFlag(InodeInfoFlag::kInlineXattr)) {
    SetInlineXattrAddrs(0);
  }

  if (GetSize() < kPageSize) {
    SetSize(kPageSize);
    SetFlag(InodeInfoFlag::kUpdateDir);
  }

  UpdateInodePage(dnode_page);
#if 0  // porting needed
  // stat_dec_inline_inode(dir);
#endif
  return ZX_OK;
}

zx::result<bool> Dir::AddInlineEntry(std::string_view name, VnodeF2fs *vnode) {
  {
    LockedPage ipage;
    if (zx_status_t err = fs()->GetNodeManager().GetNodePage(Ino(), &ipage); err != ZX_OK) {
      return zx::error(err);
    }

    f2fs_hash_t name_hash = DentryHash(name);
    uint16_t slots = GetDentrySlots(safemath::checked_cast<uint16_t>(name.length()));
    uint32_t bit_pos = RoomInInlineDir(ipage.get(), slots);
    if (bit_pos < MaxInlineDentry()) {
      ipage->WaitOnWriteback();

      if (zx_status_t err = InitInodeMetadata(vnode); err != ZX_OK) {
        if (TestFlag(InodeInfoFlag::kUpdateDir)) {
          UpdateInodePage(ipage);
          ClearFlag(InodeInfoFlag::kUpdateDir);
        }
        return zx::error(err);
      }

      DirEntry *de = &InlineDentryArray(ipage.get(), *this)[bit_pos];
      de->hash_code = name_hash;
      de->name_len = static_cast<uint16_t>(CpuToLe(name.length()));
      std::memcpy(InlineDentryFilenameArray(ipage.get(), *this)[bit_pos], name.data(),
                  name.length());
      de->ino = CpuToLe(vnode->Ino());
      SetDeType(de, vnode);
      for (int i = 0; i < slots; ++i) {
        TestAndSetBit(bit_pos + i, InlineDentryBitmap(ipage.get()));
      }

      if (de != nullptr) {
        fs()->GetDirEntryCache().UpdateDirEntry(Ino(), name, *de, kCachedInlineDirEntryPageIndex);
      }

      ipage.SetDirty();
      UpdateParentMetadata(vnode, 0);
      vnode->UpdateInodePage();
      UpdateInodePage(ipage);

      ClearFlag(InodeInfoFlag::kUpdateDir);
      return zx::ok(false);
    }
  }

  if (auto ret = ConvertInlineDir(); ret != ZX_OK) {
    return zx::error(ret);
  }
  return zx::ok(true);
}

void Dir::DeleteInlineEntry(DirEntry *dentry, fbl::RefPtr<Page> &page, VnodeF2fs *vnode) {
  LockedPage lock_page(page);
  lock_page->WaitOnWriteback();

  unsigned int bit_pos = static_cast<uint32_t>(dentry - InlineDentryArray(lock_page.get(), *this));
  int slots = GetDentrySlots(LeToCpu(dentry->name_len));
  for (int i = 0; i < slots; ++i) {
    TestAndClearBit(bit_pos + i, InlineDentryBitmap(lock_page.get()));
  }

  lock_page.SetDirty();

  std::string_view remove_name(
      reinterpret_cast<char *>(InlineDentryFilenameArray(lock_page.get(), *this)[bit_pos]),
      LeToCpu(dentry->name_len));

  fs()->GetDirEntryCache().RemoveDirEntry(Ino(), remove_name);

  timespec cur_time;
  clock_gettime(CLOCK_REALTIME, &cur_time);
  SetCTime(cur_time);
  SetMTime(cur_time);

  if (vnode && vnode->IsDir()) {
    DropNlink();
  }

  if (vnode) {
    clock_gettime(CLOCK_REALTIME, &cur_time);
    vnode->SetDirty();
    vnode->SetCTime(cur_time);
    vnode->DropNlink();
    if (vnode->IsDir()) {
      vnode->DropNlink();
      vnode->SetSize(0);
    }
    vnode->UpdateInodePage();
    if (vnode->GetNlink() == 0) {
      fs()->AddOrphanInode(vnode);
    }
  }
  UpdateInodePage(lock_page);
}

bool Dir::IsEmptyInlineDir() {
  LockedPage ipage;

  if (zx_status_t err = fs()->GetNodeManager().GetNodePage(Ino(), &ipage); err != ZX_OK)
    return false;

  unsigned int bit_pos = 2;
  bit_pos = FindNextBit(InlineDentryBitmap(ipage.get()), MaxInlineDentry(), bit_pos);

  return bit_pos >= MaxInlineDentry();
}

zx_status_t Dir::ReadInlineDir(fs::VdirCookie *cookie, void *dirents, size_t len,
                               size_t *out_actual) {
  fs::DirentFiller df(dirents, len);
  uint64_t *pos_cookie = reinterpret_cast<uint64_t *>(cookie);

  if (*pos_cookie == MaxInlineDentry()) {
    *out_actual = 0;
    return ZX_OK;
  }

  LockedPage ipage;

  if (zx_status_t err = fs()->GetNodeManager().GetNodePage(Ino(), &ipage); err != ZX_OK)
    return err;

  const unsigned char *types = kFiletypeTable;
  uint32_t bit_pos = *pos_cookie % MaxInlineDentry();

  while (bit_pos < MaxInlineDentry()) {
    bit_pos = FindNextBit(InlineDentryBitmap(ipage.get()), MaxInlineDentry(), bit_pos);
    if (bit_pos >= MaxInlineDentry()) {
      break;
    }

    DirEntry *de = &InlineDentryArray(ipage.get(), *this)[bit_pos];
    unsigned char d_type = DT_UNKNOWN;
    if (de->file_type < static_cast<uint8_t>(FileType::kFtMax))
      d_type = types[de->file_type];

    std::string_view name(
        reinterpret_cast<char *>(InlineDentryFilenameArray(ipage.get(), *this)[bit_pos]),
        LeToCpu(de->name_len));

    if (de->ino && name != "..") {
      if (zx_status_t ret = df.Next(name, d_type, LeToCpu(de->ino)); ret != ZX_OK) {
        *pos_cookie = bit_pos;

        *out_actual = df.BytesFilled();
        return ZX_OK;
      }
    }

    bit_pos += GetDentrySlots(LeToCpu(de->name_len));
  }

  *pos_cookie = MaxInlineDentry();
  *out_actual = df.BytesFilled();

  return ZX_OK;
}

zx_status_t File::ReadInline(void *data, size_t len, size_t off, size_t *out_actual) {
  LockedPage inline_page;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(Ino(), &inline_page); ret != ZX_OK) {
    return ret;
  }

  size_t cur_len = std::min(len, GetSize() - off);
  inline_page->Read(data, InlineDataOffset() + off, cur_len);

  *out_actual = cur_len;

  return ZX_OK;
}

zx_status_t File::ConvertInlineData() {
  LockedPage page;
  if (TestFlag(InodeInfoFlag::kDataExist)) {
    if (zx_status_t ret = GrabCachePage(0, &page); ret != ZX_OK) {
      return ret;
    }
  }

  LockedPage dnode_page;
  if (zx_status_t err = fs()->GetNodeManager().GetLockedDnodePage(*this, 0, &dnode_page);
      err != ZX_OK) {
    return err;
  }

  if (!TestFlag(InodeInfoFlag::kInlineData)) {
    return ZX_OK;
  }

  uint32_t ofs_in_dnode;
  if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, 0); result.is_error()) {
    return result.error_value();
  } else {
    ofs_in_dnode = result.value();
  }

  NodePage *ipage = &dnode_page.GetPage<NodePage>();
  block_t data_blkaddr = ipage->GetBlockAddr(ofs_in_dnode);
  ZX_DEBUG_ASSERT(data_blkaddr == kNullAddr);

  if (zx_status_t err = ReserveNewBlock(*ipage, ofs_in_dnode); err != ZX_OK) {
    return err;
  }

  if (TestFlag(InodeInfoFlag::kDataExist)) {
    page->WaitOnWriteback();
    page->Write(ipage->GetAddress<uint8_t>() + InlineDataOffset(), 0, GetSize());
    page.SetDirty();

    dnode_page->WaitOnWriteback();
    dnode_page.Zero(InlineDataOffset(), InlineDataOffset() + MaxInlineData());
  }
  ClearFlag(InodeInfoFlag::kInlineData);
  ClearFlag(InodeInfoFlag::kDataExist);

  UpdateInodePage(dnode_page);

  return ZX_OK;
}

zx_status_t File::WriteInline(const void *data, size_t len, size_t offset, size_t *out_actual) {
  LockedPage inline_page;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(Ino(), &inline_page); ret != ZX_OK) {
    return ret;
  }

  inline_page->WaitOnWriteback();
  inline_page->Write(data, InlineDataOffset() + offset, len);

  SetSize(std::max(static_cast<size_t>(GetSize()), offset + len));
  SetFlag(InodeInfoFlag::kDataExist);
  inline_page.SetDirty();

  timespec cur_time;
  clock_gettime(CLOCK_REALTIME, &cur_time);
  SetCTime(cur_time);
  SetMTime(cur_time);
  SetDirty();

  *out_actual = len;

  return ZX_OK;
}

zx_status_t File::TruncateInline(size_t len, bool is_recover) {
  LockedPage inline_page;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(Ino(), &inline_page); ret != ZX_OK) {
    return ret;
  }

  inline_page->WaitOnWriteback();

  size_t size_diff = (len > GetSize()) ? (len - GetSize()) : (GetSize() - len);
  size_t offset = InlineDataOffset() + ((len > GetSize()) ? GetSize() : len);
  inline_page.Zero(offset, offset + size_diff);

  // When removing inline data during recovery, file size should not be modified.
  if (!is_recover) {
    SetSize(len);
  }
  if (len == 0) {
    ClearFlag(InodeInfoFlag::kDataExist);
  }
  inline_page.SetDirty();

  timespec cur_time;
  clock_gettime(CLOCK_REALTIME, &cur_time);
  SetCTime(cur_time);
  SetMTime(cur_time);
  SetDirty();

  return ZX_OK;
}

zx_status_t File::RecoverInlineData(NodePage &page) {
  // The inline_data recovery policy is as follows.
  // [prev.] [next] of inline_data flag
  //    o       o  -> recover inline_data
  //    o       x  -> remove inline_data, and then recover data blocks
  //    x       o  -> remove data blocks, and then recover inline_data (not happen)
  //    x       x  -> recover data blocks
  // ([prev.] is checkpointed data. And [next] is data written and fsynced after checkpoint.)

  if (page.IsInode()) {
    Inode &inode = page.GetAddress<Node>()->i;

    // [next] have inline data.
    if (inode.i_inline & kInlineData) {
      // Process inline.
      LockedPage ipage;
      if (zx_status_t err = fs()->GetNodeManager().GetNodePage(Ino(), &ipage); err != ZX_OK) {
        return err;
      }
      FsBlock block;
      ipage->WaitOnWriteback();
      page.Read(block.get(), InlineDataOffset(), MaxInlineData());
      ipage->Write(block.get(), InlineDataOffset(), MaxInlineData());

      SetFlag(InodeInfoFlag::kInlineData);
      SetFlag(InodeInfoFlag::kDataExist);

      ipage.SetDirty();
      return ZX_OK;
    }
  }

  // [prev.] has inline data but [next] has no inline data.
  if (TestFlag(InodeInfoFlag::kInlineData)) {
    TruncateInline(0, true);
    ClearFlag(InodeInfoFlag::kInlineData);
    ClearFlag(InodeInfoFlag::kDataExist);
  }
  return ZX_ERR_NOT_SUPPORTED;
}

}  // namespace f2fs
