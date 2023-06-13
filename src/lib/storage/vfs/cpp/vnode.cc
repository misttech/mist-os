// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/vfs/cpp/vnode.h"

#include <zircon/assert.h>
#include <zircon/errors.h>

#include <string_view>
#include <utility>

#include "src/lib/storage/vfs/cpp/vfs.h"
#include "src/lib/storage/vfs/cpp/vfs_types.h"

#ifdef __Fuchsia__

#include <fidl/fuchsia.io/cpp/wire.h>

#include "src/lib/storage/vfs/cpp/fuchsia_vfs.h"

namespace fio = fuchsia_io;

#endif  // __Fuchsia__

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

zx_status_t Vnode::CreateStream(uint32_t stream_options, zx::stream* out_stream) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::ConnectService(zx::channel channel) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Vnode::WatchDir(FuchsiaVfs* vfs, fio::wire::WatchMask mask, uint32_t options,
                            fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::GetNodeInfo(Rights rights, VnodeRepresentation* info) {
  auto maybe_protocol = GetProtocols().which();
  ZX_DEBUG_ASSERT(maybe_protocol.has_value());
  VnodeProtocol protocol = *maybe_protocol;
  zx_status_t status = GetNodeInfoForProtocol(protocol, rights, info);
  if (status != ZX_OK) {
    return status;
  }
  switch (protocol) {
    case VnodeProtocol::kConnector:
      ZX_DEBUG_ASSERT(info->is_connector());
      break;
    case VnodeProtocol::kFile:
      ZX_DEBUG_ASSERT(info->is_file());
      break;
    case VnodeProtocol::kDirectory:
      ZX_DEBUG_ASSERT(info->is_directory());
      break;
  }
  return ZX_OK;
}

void Vnode::Notify(std::string_view name, fuchsia_io::wire::WatchEvent event) {}

zx_status_t Vnode::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx::result<std::string> Vnode::GetDevicePath() const { return zx::error(ZX_ERR_NOT_SUPPORTED); }

zx_status_t Vnode::OpenRemote(fuchsia_io::OpenFlags, fuchsia_io::ModeType, fidl::StringView,
                              fidl::ServerEnd<fuchsia_io::Node>) const {
  return ZX_ERR_NOT_SUPPORTED;
}

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

bool Vnode::Supports(VnodeProtocolSet protocols) const {
  return (GetProtocols() & protocols).any();
}

bool Vnode::ValidateRights([[maybe_unused]] Rights rights) const { return true; }

zx::result<Vnode::ValidatedOptions> Vnode::ValidateOptions(VnodeConnectionOptions options) const {
  auto protocols = options.protocols();
  if (!Supports(protocols)) {
    if (protocols == VnodeProtocol::kDirectory) {
      return zx::error(ZX_ERR_NOT_DIR);
    }
    return zx::error(ZX_ERR_NOT_FILE);
  }
  if (!ValidateRights(options.rights)) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  return zx::ok(Validated(options));
}

VnodeProtocol Vnode::Negotiate(VnodeProtocolSet protocols) const {
  auto protocol = protocols.first();
  ZX_DEBUG_ASSERT(protocol.has_value());
  return *protocol;
}

zx_status_t Vnode::Open(ValidatedOptions options, fbl::RefPtr<Vnode>* out_redirect) {
  {
    std::lock_guard lock(mutex_);
    open_count_++;
  }

  if (zx_status_t status = OpenNode(options, out_redirect); status != ZX_OK) {
    // Roll back the open count since we won't get a close for it.
    std::lock_guard lock(mutex_);
    open_count_--;
    return status;
  }
  return ZX_OK;
}

zx_status_t Vnode::OpenValidating(VnodeConnectionOptions options,
                                  fbl::RefPtr<Vnode>* out_redirect) {
  auto validated_options = ValidateOptions(options);
  if (validated_options.is_error()) {
    return validated_options.status_value();
  }
  // The documentation on Vnode::Open promises it will never be called if options includes
  // vnode_reference.
  ZX_DEBUG_ASSERT(!validated_options.value()->flags.node_reference);
  return Open(validated_options.value(), out_redirect);
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

void Vnode::DidModifyStream() {}

bool Vnode::SupportsClientSideStreams() { return false; }

zx_status_t Vnode::Lookup(std::string_view name, fbl::RefPtr<Vnode>* out) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::GetAttributes(VnodeAttributes* a) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Vnode::SetAttributes(VnodeAttributesUpdate a) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Vnode::Readdir(VdirCookie* cookie, void* dirents, size_t len, size_t* out_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Vnode::Create(std::string_view name, uint32_t mode, fbl::RefPtr<Vnode>* out) {
  return ZX_ERR_NOT_SUPPORTED;
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

DirentFiller::DirentFiller(void* ptr, size_t len)
    : ptr_(static_cast<char*>(ptr)), pos_(0), len_(len) {}

zx_status_t DirentFiller::Next(std::string_view name, uint8_t type, uint64_t ino) {
  vdirent_t* de = reinterpret_cast<vdirent_t*>(ptr_ + pos_);
  size_t sz = sizeof(vdirent_t) + name.length();

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
