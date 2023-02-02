// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

File::File(F2fs *fs, ino_t ino, umode_t mode) : VnodeF2fs(fs, ino, mode) {}

#if 0  // porting needed
// int File::F2fsVmPageMkwrite(vm_area_struct *vma, vm_fault *vmf) {
//   return 0;
//   //   Page *page = vmf->page;
//   //   VnodeF2fs *vnode = this;
//   //   // SuperblockInfo &superblock_info = Vfs()->GetSuperblockInfo();
//   //   Page *node_page;
//   //   block_t old_blk_addr;
//   //   DnodeOfData dn;
//   //   int err;

//   //   Vfs()->GetSegmentManager().BalanceFs();

//   //   sb_start_pagefault(nullptr);

//   //   // superblock_info->mutex_lock_op(LockType::kDataNew);

//   //   /* block allocation */
//   //   SetNewDnode(&dn, vnode, nullptr, nullptr, 0);
//   //   err = Vfs()->GetNodeManager().GetDnodeOfData(&dn, page->index, 0);
//   //   if (err) {
//   //     // superblock_info->mutex_unlock_op(LockType::kDataNew);
//   //     goto out;
//   //   }

//   //   old_blk_addr = dn.data_blkaddr;
//   //   node_page = dn.node_page;

//   //   if (old_blk_addr == kNullAddr) {
//   //     err = VnodeF2fs::ReserveNewBlock(&dn);
//   //     if (err) {
//   //       F2fsPutDnode(&dn);
//   //       // superblock_info->mutex_unlock_op(LockType::kDataNew);
//   //       goto out;
//   //     }
//   //   }
//   //   F2fsPutDnode(&dn);

//   //   // superblock_info->mutex_unlock_op(LockType::kDataNew);

//   //   // lock_page(page);
//   //   // if (page->mapping != inode->i_mapping ||
//   //   //     page_offset(page) >= i_size_read(inode) ||
//   //   //     !PageUptodate(page)) {
//   //   //   unlock_page(page);
//   //   //   err = -EFAULT;
//   //   //   goto out;
//   //   // }

//   //   /*
//   //    * check to see if the page is mapped already (no holes)
//   //    */
//   //   if (PageMappedToDisk(page))
//   //     goto out;

//   //   /* fill the page */
//   //   WaitOnPageWriteback(page);

//   //   /* page is wholly or partially inside EOF */
//   //   if (((page->index + 1) << kPageCacheShift) > i_size) {
//   //     unsigned offset;
//   //     offset = i_size & ~PAGE_CACHE_MASK;
//   //     ZeroUserSegment(page, offset, kPageSize);
//   //   }
//   //   // set_page_dirty(page);
//   //   FlushDirtyDataPage(Vfs(), *page);
//   //   SetPageUptodate(page);

//   //   file_update_time(vma->vm_file);
//   // out:
//   //   sb_end_pagefault(nullptr);
//   //   return block_page_mkwrite_return(err);
// }
#endif

#if 0  // porting needed
// void File::FillZero(pgoff_t index, loff_t start, loff_t len) {
//   Page *page = nullptr;
//   zx_status_t err;

//   if (!len)
//     return;

//   err = GetNewDataPage(index, false, page);

//   if (!err) {
//     WaitOnPageWriteback(page);
//     zero_user(page, start, len);
// #if 0  // porting needed
//     // set_page_dirty(page);
// #else
//     FlushDirtyDataPage(Vfs(), *page);
// #endif
//     Page::PutPage(page, 1);
//   }
// }

// int File::PunchHole(loff_t offset, loff_t len, int mode) {
//   pgoff_t pg_start, pg_end;
//   loff_t off_start, off_end;
//   int ret = 0;

//   pg_start = ((uint64_t)offset) >> kPageCacheShift;
//   pg_end = ((uint64_t)offset + len) >> kPageCacheShift;

//   off_start = offset & (kPageSize - 1);
//   off_end = (offset + len) & (kPageSize - 1);

//   if (pg_start == pg_end) {
//     FillZero(pg_start, off_start, off_end - off_start);
//   } else {
//     if (off_start)
//       FillZero(pg_start++, off_start, kPageSize - off_start);
//     if (off_end)
//       FillZero(pg_end, 0, off_end);

//     if (pg_start < pg_end) {
//       loff_t blk_start, blk_end;

//       blk_start = pg_start << kPageCacheShift;
//       blk_end = pg_end << kPageCacheShift;
// #if 0  // porting needed
//       // truncate_inode_pages_range(nullptr, blk_start,
//       //     blk_end - 1);
// #endif
//       ret = TruncateHole(pg_start, pg_end);
//     }
//   }

//   if (!(mode & FALLOC_FL_KEEP_SIZE) && i_size <= (offset + len)) {
//     i_size = offset;
//     MarkInodeDirty();
//   }

//   return ret;
// }

// int File::ExpandInodeData(loff_t offset, off_t len, int mode) {
//   SuperblockInfo &superblock_info = Vfs()->GetSuperblockInfo();
//   pgoff_t pg_start, pg_end;
//   loff_t new_size = i_size;
//   loff_t off_start, off_end;
//   int ret = 0;

// #if 0  // porting needed
//   // ret = inode_newsize_ok(this, (len + offset));
//   // if (ret)
//   //   return ret;
// #endif

//   pg_start = ((uint64_t)offset) >> kPageCacheShift;
//   pg_end = ((uint64_t)offset + len) >> kPageCacheShift;

//   off_start = offset & (kPageSize - 1);
//   off_end = (offset + len) & (kPageSize - 1);

//   for (pgoff_t index = pg_start; index <= pg_end; ++index) {
//     DnodeOfData dn;

//     superblock_info.mutex_lock_op(LockType::kDataNew);

//     SetNewDnode(&dn, this, nullptr, nullptr, 0);
//     ret = Vfs()->GetNodeManager().GetDnodeOfData(&dn, index, 0);
//     if (ret) {
//       superblock_info.mutex_unlock_op(LockType::kDataNew);
//       break;
//     }

//     if (dn.data_blkaddr == kNullAddr) {
//       ret = ReserveNewBlock(&dn);
//       if (ret) {
//         F2fsPutDnode(&dn);
//         superblock_info.mutex_unlock_op(LockType::kDataNew);
//         break;
//       }
//     }
//     F2fsPutDnode(&dn);

//     superblock_info.mutex_unlock_op(LockType::kDataNew);

//     if (pg_start == pg_end)
//       new_size = offset + len;
//     else if (index == pg_start && off_start)
//       new_size = (index + 1) << kPageCacheShift;
//     else if (index == pg_end)
//       new_size = (index << kPageCacheShift) + off_end;
//     else
//       new_size += kPageSize;
//   }

//   if (!(mode & FALLOC_FL_KEEP_SIZE) && i_size < new_size) {
//     i_size = new_size;
//     MarkInodeDirty();
//   }

//   return ret;
// }

// long File::F2fsFallocate(int mode, loff_t offset, loff_t len) {
//   long ret;

//   if (mode & ~(FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE))
//     return -EOPNOTSUPP;

//   if (mode & FALLOC_FL_PUNCH_HOLE)
//     ret = PunchHole(offset, len, mode);
//   else
//     ret = ExpandInodeData(offset, len, mode);

//   Vfs()->GetSegmentManager().BalanceFs();
//   return ret;
// }

// #define F2FS_REG_FLMASK    (~(FS_DIRSYNC_FL | FS_TOPDIR_FL))
// #define F2FS_OTHER_FLMASK  (FS_NODUMP_FL | FS_NOATIME_FL)

// inline uint32_t File::F2fsMaskFlags(umode_t mode, uint32_t flags) {
//   if (S_ISDIR(mode))
//     return flags;
//   else if (S_ISREG(mode))
//     return flags & F2FS_REG_FLMASK;
//   else
//     return flags & F2FS_OTHER_FLMASK;
// }

// long File::F2fsIoctl(/*file *filp,*/ unsigned int cmd, uint64_t arg) {
  //   inode *inode = filp->f_dentry->d_inode;
  //   InodeInfo *fi = F2FS_I(inode);
  //   unsigned int flags;
  //   int ret;

  //   switch (cmd) {
  //   case FS_IOC_GETFLAGS:
  //     flags = fi->i_flags & FS_FL_USER_VISIBLE;
  //     return put_user(flags, (int __user *) arg);
  //   case FS_IOC_SETFLAGS:
  //   {
  //     unsigned int oldflags;

  //     ret = mnt_want_write(filp->f_path.mnt);
  //     if (ret)
  //       return ret;

  //     if (!inode_owner_or_capable(inode)) {
  //       ret = -EACCES;
  //       goto out;
  //     }

  //     if (get_user(flags, (int __user *) arg)) {
  //       ret = -EFAULT;
  //       goto out;
  //     }

  //     flags = f2fs_mask_flags(inode->i_mode, flags);

  //     mutex_lock(&inode->i_mutex_);

  //     oldflags = fi->i_flags;

  //     if ((flags ^ oldflags) & (FS_APPEND_FL | FS_IMMUTABLE_FL)) {
  //       if (!capable(CAP_LINUX_IMMUTABLE)) {
  //         mutex_unlock(&inode->i_mutex_);
  //         ret = -EPERM;
  //         goto out;
  //       }
  //     }

  //     flags = flags & FS_FL_USER_MODIFIABLE;
  //     flags |= oldflags & ~FS_FL_USER_MODIFIABLE;
  //     fi->i_flags = flags;
  //     mutex_unlock(&inode->i_mutex_);

  //     f2fs_set_inode_flags(inode);
  //     inode->i_ctime = CURRENT_TIME;
  //     MarkInodeDirty(inode);
  // out:
  //     mnt_drop_write(filp->f_path.mnt);
  //     return ret;
  //   }
  //   default:
  //     return -ENOTTY;
  //   }
// }
#endif

zx_status_t File::Read(void *data, size_t length, size_t offset, size_t *out_actual) {
  TRACE_DURATION("f2fs", "File::Read", "event", "File::Read", "ino", Ino(), "offset",
                 offset / kBlockSize, "length", length / kBlockSize);

  if (offset >= GetSize()) {
    *out_actual = 0;
    return ZX_OK;
  }

  size_t num_bytes = std::min(length, GetSize() - offset);
  if (TestFlag(InodeInfoFlag::kInlineData)) {
    return ReadInline(data, num_bytes, offset, out_actual);
  }

  const pgoff_t vmo_node_start = safemath::CheckDiv<pgoff_t>(offset, kVmoNodeSize).ValueOrDie();
  const pgoff_t vmo_node_end =
      CheckedDivRoundUp<pgoff_t>(safemath::CheckAdd(offset, num_bytes).ValueOrDie(), kVmoNodeSize);
  size_t src_offset = safemath::CheckMod(offset, kVmoNodeSize).ValueOrDie();
  size_t dst_offset = 0;

  for (size_t node = vmo_node_start; node < vmo_node_end && num_bytes; ++node) {
    VmoHolder vmo_node(vmo_manager(), kBlocksPerVmoNode * node);
    uint8_t *const addr = static_cast<uint8_t *>(vmo_node.GetAddress());
    size_t num_bytes_in_node = safemath::CheckSub<size_t>(kVmoNodeSize, src_offset).ValueOrDie();
    num_bytes_in_node = std::min(num_bytes_in_node, num_bytes);
    std::memcpy(static_cast<uint8_t *>(data) + dst_offset, &addr[src_offset], num_bytes_in_node);

    dst_offset += num_bytes_in_node;
    num_bytes -= num_bytes_in_node;
    src_offset = 0;
  }

  *out_actual = dst_offset;

  return ZX_OK;
}

zx_status_t File::DoWrite(const void *data, size_t len, size_t offset, size_t *out_actual) {
  if (len == 0) {
    return ZX_OK;
  }

  if (offset + len > static_cast<size_t>(MaxFileSize(fs()->RawSb().log_blocksize))) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (TestFlag(InodeInfoFlag::kInlineData)) {
    if (offset + len < MaxInlineData()) {
      return WriteInline(data, len, offset, out_actual);
    }

    ConvertInlineData();
  }

  fs()->GetSegmentManager().BalanceFs();
  if (auto status = WriteBegin(offset, len); status.is_error()) {
    return status.error_value();
  }

  size_t num_bytes = len;
  const pgoff_t vmo_node_start = safemath::CheckDiv<pgoff_t>(offset, kVmoNodeSize).ValueOrDie();
  const pgoff_t vmo_node_end =
      CheckedDivRoundUp<pgoff_t>(safemath::CheckAdd(offset, num_bytes).ValueOrDie(), kVmoNodeSize);
  size_t dst_offset = safemath::CheckMod(offset, kVmoNodeSize).ValueOrDie();
  size_t src_offset = 0;

  for (size_t node = vmo_node_start; node < vmo_node_end && num_bytes; ++node) {
    VmoHolder vmo_node(vmo_manager(), kBlocksPerVmoNode * node);
    uint8_t *const addr = static_cast<uint8_t *>(vmo_node.GetAddress());
    size_t num_bytes_in_node = safemath::CheckSub<size_t>(kVmoNodeSize, dst_offset).ValueOrDie();
    num_bytes_in_node = std::min(num_bytes_in_node, num_bytes);
    std::memcpy(&addr[dst_offset], static_cast<const uint8_t *>(data) + src_offset,
                num_bytes_in_node);

    src_offset += num_bytes_in_node;
    num_bytes -= num_bytes_in_node;
    dst_offset = 0;
  }

  if (src_offset > 0) {
    if (offset + src_offset > GetSize()) {
      SetSize(offset + src_offset);
    }
    timespec cur_time;
    clock_gettime(CLOCK_REALTIME, &cur_time);
    SetCTime(cur_time);
    SetMTime(cur_time);
    MarkInodeDirty(true);
  }

  *out_actual = src_offset;

  return ZX_OK;
}

zx_status_t File::Write(const void *data, size_t len, size_t offset, size_t *out_actual) {
  TRACE_DURATION("f2fs", "File::Write", "event", "File::Write", "ino", Ino(), "offset",
                 offset / kBlockSize, "length", len / kBlockSize);

  if (fs()->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }
  return DoWrite(data, len, offset, out_actual);
}

zx_status_t File::Append(const void *data, size_t len, size_t *out_end, size_t *out_actual) {
  size_t off = GetSize();
  TRACE_DURATION("f2fs", "File::Append", "event", "File::Append", "ino", Ino(), "offset", off,
                 "length", len);

  if (fs()->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag)) {
    *out_end = off;
    return ZX_ERR_BAD_STATE;
  }
  zx_status_t ret = DoWrite(data, len, off, out_actual);
  *out_end = off + *out_actual;
  return ret;
}

zx_status_t File::Truncate(size_t len) {
  TRACE_DURATION("f2fs", "File::Truncate", "event", "File::Truncate", "ino", Ino(), "length", len);

  if (fs()->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  if (len == GetSize())
    return ZX_OK;

  if (len > static_cast<size_t>(MaxFileSize(fs()->RawSb().log_blocksize)))
    return ZX_ERR_INVALID_ARGS;

  if (TestFlag(InodeInfoFlag::kInlineData)) {
    if (len < MaxInlineData()) {
      return TruncateInline(len, false);
    }

    ConvertInlineData();
  }

  return DoTruncate(len);
}

loff_t File::MaxFileSize(unsigned bits) {
  loff_t result = GetAddrsPerInode();
  loff_t leaf_count = kAddrsPerBlock;

  // two direct node blocks
  result += (leaf_count * 2);

  // two indirect node blocks
  leaf_count *= kNidsPerBlock;
  result += (leaf_count * 2);

  // one double indirect node block
  leaf_count *= kNidsPerBlock;
  result += leaf_count;

  result <<= bits;
  return result;
}

zx_status_t File::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo *out_vmo) {
  if (flags & fuchsia_io::wire::VmoFlags::kExecute) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // mmapped vnodes are not allowed to have inline data.
  if (TestFlag(InodeInfoFlag::kInlineData)) {
    ConvertInlineData();
  }
  return VnodeF2fs::GetVmo(flags, out_vmo);
}

void File::VmoDirty(uint64_t offset, uint64_t length) {
  // If kInlineData is set, it means that ConvertInlineData() is handling block allocation and
  // migrating inline data to the block at |offset|. Just make this range dirty.
  if (!TestFlag(InodeInfoFlag::kInlineData) && !TestFlag(InodeInfoFlag::kNoAlloc)) {
    // File::DoWrite() has already allocated its block. When F2fs supports zx::stream, every block
    // will be allocated here.
    if (auto status = WriteBegin(offset, length, true); status.is_error()) {
      FX_LOGS(WARNING) << "failed to reserve blocks at " << offset << " + " << length << ", "
                       << status.status_string();
      fs::SharedLock rlock(mutex_);
      return ReportPagerError(offset, length, status.status_value());
    }
  }
  return VnodeF2fs::VmoDirty(offset, length);
}

}  // namespace f2fs
