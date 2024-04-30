// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_DIR_H_
#define SRC_STORAGE_F2FS_DIR_H_

#include "src/storage/f2fs/common.h"
#include "src/storage/f2fs/vnode.h"

namespace f2fs {

struct DirHash {
  f2fs_hash_t hash = 0;  // hash value of given file name
  uint32_t level = 0;    // maximum level of given file name
};

extern const unsigned char kFiletypeTable[];
f2fs_hash_t DentryHash(std::string_view name);
uint64_t DirBlockIndex(uint32_t level, uint8_t dir_level, uint32_t idx);

class Dir : public VnodeF2fs, public fbl::Recyclable<Dir> {
 public:
  explicit Dir(F2fs *fs, ino_t ino, umode_t mode);

  // Required for memory management, see the class comment above Vnode for more.
  void fbl_recycle() { RecycleNode(); }

  fuchsia_io::NodeProtocolKinds GetProtocols() const final;

  zx::result<LockedPage> FindDataPage(pgoff_t index, bool do_read = true);

  // Lookup
  zx_status_t Lookup(std::string_view name, fbl::RefPtr<fs::Vnode> *out) final
      __TA_EXCLUDES(mutex_);

  zx_status_t DoLookup(std::string_view name, fbl::RefPtr<fs::Vnode> *out)
      __TA_REQUIRES_SHARED(mutex_);
  zx::result<DirEntry> LookUpEntries(std::string_view name) __TA_REQUIRES_SHARED(mutex_);
  std::pair<DirEntry *, DirHash> FindEntryOnDevice(std::string_view name,
                                                   fbl::RefPtr<Page> *res_page)
      __TA_REQUIRES_SHARED(mutex_);
  DirEntry *FindEntry(std::string_view name, fbl::RefPtr<Page> *res_page) __TA_REQUIRES(mutex_);
  zx::result<DirEntry> FindEntry(std::string_view name) __TA_REQUIRES(mutex_);
  DirEntry *FindInInlineDir(std::string_view name, fbl::RefPtr<Page> *res_page)
      __TA_REQUIRES_SHARED(mutex_);
  DirEntry *FindInBlock(fbl::RefPtr<Page> dentry_page, std::string_view name, uint64_t *max_slots,
                        f2fs_hash_t namehash, fbl::RefPtr<Page> *res_page);
  DirEntry *FindInLevel(unsigned int level, std::string_view name, f2fs_hash_t namehash,
                        bool *update_hash, fbl::RefPtr<Page> *res_page)
      __TA_REQUIRES_SHARED(mutex_);
  zx_status_t Readdir(fs::VdirCookie *cookie, void *dirents, size_t len, size_t *out_actual) final
      __TA_EXCLUDES(mutex_);
  zx_status_t ReadInlineDir(fs::VdirCookie *cookie, void *dirents, size_t len, size_t *out_actual)
      __TA_REQUIRES_SHARED(mutex_);

  // rename
  zx_status_t Rename(fbl::RefPtr<fs::Vnode> _newdir, std::string_view oldname,
                     std::string_view newname, bool src_must_be_dir, bool dst_must_be_dir) final
      __TA_EXCLUDES(mutex_, f2fs::GetGlobalLock());
  void SetLink(DirEntry *de, fbl::RefPtr<Page> &page, VnodeF2fs *vnode) __TA_REQUIRES(mutex_);
  DirEntry *ParentDir(fbl::RefPtr<Page> *out) __TA_EXCLUDES(mutex_);
  DirEntry *ParentInlineDir(fbl::RefPtr<Page> *out) __TA_REQUIRES_SHARED(mutex_);

  // create and link
  zx_status_t Link(std::string_view name, fbl::RefPtr<fs::Vnode> new_child) final
      __TA_EXCLUDES(mutex_, f2fs::GetGlobalLock());
  zx::result<fbl::RefPtr<fs::Vnode>> Create(std::string_view name, fs::CreationType type) final
      __TA_EXCLUDES(mutex_, f2fs::GetGlobalLock());
  zx::result<fbl::RefPtr<fs::Vnode>> CreateWithMode(std::string_view name, umode_t mode)
      __TA_EXCLUDES(mutex_, f2fs::GetGlobalLock());
  zx_status_t DoCreate(std::string_view name, umode_t mode, fbl::RefPtr<fs::Vnode> *out)
      __TA_REQUIRES(mutex_) __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx_status_t NewInode(umode_t mode, fbl::RefPtr<VnodeF2fs> *out) __TA_REQUIRES(mutex_);
  zx_status_t Mkdir(std::string_view name, umode_t mode, fbl::RefPtr<fs::Vnode> *out)
      __TA_REQUIRES(mutex_) __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx_status_t AddLink(std::string_view name, VnodeF2fs *vnode) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx::result<bool> AddInlineEntry(std::string_view name, VnodeF2fs *vnode) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx_status_t ConvertInlineDir() __TA_REQUIRES(mutex_) __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  void UpdateParentMetadata(VnodeF2fs *vnode, unsigned int current_depth) __TA_REQUIRES(mutex_);
  zx_status_t InitInodeMetadata(VnodeF2fs *vnode) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx_status_t MakeEmpty(VnodeF2fs *vnode) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx_status_t MakeEmptyInlineDir(VnodeF2fs *vnode) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  void InitDentInode(VnodeF2fs *vnode, NodePage &page) __TA_REQUIRES(mutex_);
  size_t RoomInInlineDir(const PageBitmap &bits, size_t slots) __TA_REQUIRES_SHARED(mutex_);
  size_t RoomForFilename(const PageBitmap &bits, size_t slots) __TA_REQUIRES_SHARED(mutex_);

  // delete
  zx_status_t Unlink(std::string_view name, bool must_be_dir) final
      __TA_EXCLUDES(mutex_, f2fs::GetGlobalLock());
  zx_status_t Rmdir(Dir *vnode, std::string_view name) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  zx_status_t DoUnlink(VnodeF2fs *vnode, std::string_view name) __TA_REQUIRES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  void DeleteEntry(DirEntry *dentry, fbl::RefPtr<Page> &page, VnodeF2fs *vnode)
      __TA_REQUIRES(mutex_) __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  void DeleteInlineEntry(DirEntry *dentry, fbl::RefPtr<Page> &page, VnodeF2fs *vnode)
      __TA_REQUIRES(mutex_) __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());

  // recovery
  zx::result<> RecoverLink(VnodeF2fs &vnode) __TA_EXCLUDES(mutex_, f2fs::GetGlobalLock());

  zx_status_t GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo *out_vmo) final {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void VmoRead(uint64_t offset, uint64_t length) final;
  zx::result<PageBitmap> GetBitmap(fbl::RefPtr<Page> dentry_page) final;

  block_t GetBlockAddr(LockedPage &page) final;

 private:
  // helper
  block_t DirBlocks();
  void SetDeType(DirEntry *de, VnodeF2fs *vnode);
  bool EarlyMatchName(std::string_view name, f2fs_hash_t namehash, const DirEntry &de);
  zx::result<bool> IsSubdir(Dir *possible_dir);
  bool IsEmptyDir();
  bool IsEmptyInlineDir();

  // inline helper
  uint64_t InlineDentryBitmapSize() const;
  uint8_t *InlineDentryBitmap(Page *page);
  DirEntry *InlineDentryArray(Page *page, VnodeF2fs &vnode);
  uint8_t (*InlineDentryFilenameArray(Page *page, VnodeF2fs &vnode))[kDentrySlotLen];

  // link helper to update link information in Rename()
  DirEntry *FindEntrySafe(std::string_view name, fbl::RefPtr<Page> *res_page) __TA_EXCLUDES(mutex_);
  zx_status_t AddLinkSafe(std::string_view name, VnodeF2fs *vnode) __TA_EXCLUDES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());
  void SetLinkSafe(DirEntry *de, fbl::RefPtr<Page> &page, VnodeF2fs *vnode) __TA_EXCLUDES(mutex_)
      __TA_REQUIRES_SHARED(f2fs::GetGlobalLock());

  DirHash cached_hash_ __TA_GUARDED(mutex_);

  void SetDirHash(const DirHash &hash) __TA_REQUIRES(mutex_) { cached_hash_ = hash; }

#if 0  // porting needed
//   int F2fsSymlink(dentry *dentry, const char *symname);
//   int F2fsMknod(dentry *dentry, umode_t mode, dev_t rdev);
#endif
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_DIR_H_
