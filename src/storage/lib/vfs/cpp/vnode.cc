// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vnode.h"

#include <zircon/assert.h>
#include <zircon/errors.h>

#include <string_view>
#include <utility>

#include "src/storage/lib/vfs/cpp/vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

#ifdef __Fuchsia__

#include <fidl/fuchsia.io/cpp/wire.h>

#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"

#endif  // __Fuchsia__

namespace fio = fuchsia_io;

namespace fs {

#ifdef __Fuchsia__
std::mutex Vnode::gLockAccess;
std::map<const Vnode*, std::shared_ptr<file_lock::FileLock>> Vnode::gLockMap;
#endif

#ifdef __Fuchsia__
Vnode::~Vnode() {
  ZX_DEBUG_ASSERT_MSG(gLockMap.find(this) == gLockMap.end(),
                      "lock entry in gLockMap not cleaned up for Vnode");
}
#else
Vnode::~Vnode() = default;
#endif

#ifdef __Fuchsia__

zx::result<zx::stream> Vnode::CreateStream(uint32_t stream_options) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t Vnode::ConnectService(zx::channel channel) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Vnode::WatchDir(FuchsiaVfs* vfs, fio::wire::WatchMask mask, uint32_t options,
                            fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo) {
  return ZX_ERR_NOT_SUPPORTED;
}

void Vnode::DeprecatedOpenRemote(fuchsia_io::OpenFlags, fuchsia_io::ModeType, fidl::StringView,
                                 fidl::ServerEnd<fuchsia_io::Node>) const {
  ZX_PANIC("OpenRemote should only be called on remote nodes!");
}

#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
void Vnode::OpenRemote(fuchsia_io::wire::DirectoryOpenRequest request) const {
  ZX_PANIC("OpenRemote should only be called on remote nodes!");
}
#else
void Vnode::OpenRemote(fuchsia_io::wire::DirectoryOpen3Request request) const {
  ZX_PANIC("OpenRemote should only be called on remote nodes!");
}
#endif

std::shared_ptr<file_lock::FileLock> Vnode::GetVnodeFileLock() {
  std::lock_guard lock_access(gLockAccess);
  auto lock = gLockMap.find(this);
  if (lock == gLockMap.end()) {
    auto inserted = gLockMap.emplace(std::pair(this, std::make_shared<file_lock::FileLock>()));
    if (inserted.second) {
      lock = inserted.first;
    } else {
      return nullptr;
    }
  }
  return lock->second;
}

bool Vnode::DeleteFileLock(zx_koid_t owner) {
  std::lock_guard lock_access(gLockAccess);
  bool deleted = false;
  auto lock = gLockMap.find(this);
  if (lock != gLockMap.end()) {
    deleted = lock->second->Forget(owner);
    if (lock->second->NoLocksHeld()) {
      gLockMap.erase(this);
    }
  }
  return deleted;
}

// There is no guard here, as the connection is in teardown.
bool Vnode::DeleteFileLockInTeardown(zx_koid_t owner) {
  if (gLockMap.find(this) == gLockMap.end()) {
    return false;
  }
  return DeleteFileLock(owner);
}

#endif  // __Fuchsia__

bool Vnode::ValidateRights([[maybe_unused]] fuchsia_io::Rights rights) const { return true; }

zx::result<> Vnode::ValidateOptions(VnodeConnectionOptions options) const {
  // The connection should ensure only one of DIRECTORY and NOT_DIRECTORY is set.
  ZX_DEBUG_ASSERT(!((options.flags & fuchsia_io::OpenFlags::kDirectory) &&
                    options.flags & fuchsia_io::OpenFlags::kNotDirectory));
  if (!Supports(options.protocols())) {
    if (options.protocols() & fuchsia_io::NodeProtocolKinds::kDirectory) {
      return zx::error(ZX_ERR_NOT_DIR);
    }
    return zx::error(ZX_ERR_NOT_FILE);
  }
  if (!ValidateRights(options.rights)) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  return zx::ok();
}

zx_status_t Vnode::Open(fbl::RefPtr<Vnode>* out_redirect) {
  {
    std::lock_guard lock(mutex_);
    open_count_++;
  }

  if (zx_status_t status = OpenNode(out_redirect); status != ZX_OK) {
    // Roll back the open count since we won't get a close for it.
    std::lock_guard lock(mutex_);
    open_count_--;
    return status;
  }
  return ZX_OK;
}

zx_status_t Vnode::Close() {
  {
    std::lock_guard lock(mutex_);
    open_count_--;
  }
  return CloseNode();
}

zx_status_t Vnode::Read(void* data, size_t len, size_t off, size_t* out_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::Write(const void* data, size_t len, size_t offset, size_t* out_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::Append(const void* data, size_t len, size_t* out_end, size_t* out_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::Lookup(std::string_view name, fbl::RefPtr<Vnode>* out) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx::result<fs::VnodeAttributes> Vnode::GetAttributes() const {
  // Return the empty set of attributes by default.
  return zx::ok(fs::VnodeAttributes{});
}

zx::result<> Vnode::UpdateAttributes(const VnodeAttributesUpdate&) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t Vnode::Readdir(VdirCookie* cookie, void* dirents, size_t len, size_t* out_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx::result<fbl::RefPtr<Vnode>> Vnode::Create(std::string_view name, CreationType type) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t Vnode::Unlink(std::string_view name, bool must_be_dir) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Vnode::Truncate(size_t len) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Vnode::Rename(fbl::RefPtr<Vnode> newdir, std::string_view oldname,
                          std::string_view newname, bool src_must_be_dir, bool dst_must_be_dir) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::Link(std::string_view name, fbl::RefPtr<Vnode> target) {
  return ZX_ERR_NOT_SUPPORTED;
}

void Vnode::Sync(SyncCallback closure) { closure(ZX_ERR_NOT_SUPPORTED); }

bool Vnode::IsRemote() const { return false; }

fio::Abilities Vnode::GetAbilities() const {
  fio::Abilities abilities = fio::Abilities::kGetAttributes;
  if (SupportedMutableAttributes()) {
    abilities |= fio::Abilities::kUpdateAttributes;
  }
  fio::NodeProtocolKinds protocols = GetProtocols();
  if (protocols & fio::NodeProtocolKinds::kDirectory) {
    abilities |=
        fio::Abilities::kModifyDirectory | fio::Abilities::kTraverse | fio::Abilities::kEnumerate;
  }
  if (protocols & fio::NodeProtocolKinds::kFile) {
    abilities |= fio::Abilities::kReadBytes | fio::Abilities::kWriteBytes;
  }
  return abilities;
}

DirentFiller::DirentFiller(void* ptr, size_t len)
    : ptr_(static_cast<char*>(ptr)), pos_(0), len_(len) {}

zx_status_t DirentFiller::Next(std::string_view name, uint8_t type, uint64_t ino) {
// TODO(b/293947862): Remove use of deprecated `vdirent_t` when transitioning ReadDir to Enumerate
// as part of io2 migration.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
  vdirent_t* de = reinterpret_cast<vdirent_t*>(ptr_ + pos_);
  size_t sz = sizeof(vdirent_t) + name.length();
#pragma clang diagnostic pop

  if (sz > len_ - pos_ || name.length() > NAME_MAX) {
    return ZX_ERR_INVALID_ARGS;
  }
  de->ino = ino;
  de->size = static_cast<uint8_t>(name.length());
  de->type = type;
  memcpy(de->name, name.data(), name.length());
  pos_ += sz;
  return ZX_OK;
}

}  // namespace fs
