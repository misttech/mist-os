// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vfs.h"

#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include <string_view>
#include <utility>

#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {
namespace {

// Traverse the directory tree starting at |vndir| until the last component in |path|, or until a
// remote mount point is encountered. Both |path| and |vndir| are updated in-place.
//
// On success, |path| will be the canonical name for the entry to lookup within |vndir|. Note that
// on Fuchsia, the dot path (".") is used as the canonical form for a reference to |vndir| itself.
//
// See https://fxbug.dev/42103076 for a discussion on this mapping.
zx_status_t Traverse(fbl::RefPtr<Vnode>& vndir, std::string_view& path) {
  if (path.empty() || path.length() >= fio::kMaxPathLength) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Handle "." and "/", ensuring we map the latter to the canonical form used by the VFS.
  if (path == "." || path == "/") {
    path = ".";
    return ZX_OK;
  }

  // Allow a single leading '/'.
  if (path[0] == '/') {
    path = path.substr(1);
  }
  // Allow trailing '/', but only if preceded by something.
  if (path.length() > 1 && path.back() == '/') {
    path = path.substr(0, path.length() - 1);
  }

  while (!path.empty()) {
    // If we hit a remote mount point, the caller must forward the remainder of |path| there.
    if (vndir->IsRemote()) {
      return ZX_OK;
    }
    // Look for the next '/' separated path component.
    size_t slash = path.find('/');
    std::string_view component = path.substr(0, slash);
    if (component.length() > fio::kMaxNameLength) {
      return ZX_ERR_BAD_PATH;  // Maps to ENAMETOOLONG
    }
    if (component.empty() || component == "." || component == "..") {
      // Clients are required to transform paths into their canonical form.
      return ZX_ERR_INVALID_ARGS;
    }
    // Stop traversal if this is the last component (e.g. there are no remaining '/' separators).
    if (slash == std::string_view::npos) {
      return ZX_OK;
    }
    // Traverse to the next component, updating |vndir| and |path| in-place.
    fbl::RefPtr<fs::Vnode> next_vn;
    if (zx_status_t status = vndir->Lookup(component, &next_vn); status != ZX_OK) {
      return status;
    }
    vndir = std::move(next_vn);
    path = path.substr(slash + 1);
  }
  return ZX_ERR_INVALID_ARGS;
}

}  // namespace

Vfs::Vfs() = default;

Vfs::OpenResult Vfs::Open(fbl::RefPtr<Vnode> vndir, std::string_view path,
                          VnodeConnectionOptions options, fuchsia_io::Rights connection_rights) {
  FS_PRETTY_TRACE_DEBUG("Vfs::Open: path='", path, "' options=", options,
                        ", connection_rights=", connection_rights);
  std::lock_guard lock(vfs_lock_);
  // Traverse directory tree until last component, updating |vndir| and |path| in-place.
  if (zx_status_t status = Traverse(vndir, path); status != ZX_OK) {
    return status;
  }
  if (vndir->IsRemote()) {
    // remote filesystem, return handle and path to caller
    return OpenResult::Remote{.vnode = std::move(vndir), .path = path};
  }
  // |Traverse()| should guarantee |path| is only a single and valid component.
  ZX_DEBUG_ASSERT(!path.empty() && path.find('/') == std::string_view::npos && path != "..");

  fbl::RefPtr<Vnode> vn;
  bool vn_is_open;
  {
    CreationMode mode = internal::CreationModeFromFidl(options.flags);
    std::optional<CreationType> type = std::nullopt;
    if (mode != CreationMode::kNever) {
      type = options.flags & fio::OpenFlags::kDirectory ? CreationType::kDirectory
                                                        : CreationType::kFile;
    }
    zx::result result = CreateOrLookup(std::move(vndir), path, mode, type, connection_rights);
    if (result.is_error()) {
      return result.error_value();
    }
    std::tie(vn, vn_is_open) = *std::move(result);
  }

  if (vn->IsRemote()) {
    // Opening a mount point: Traverse across remote.
    return OpenResult::Remote{.vnode = std::move(vn), .path = "."};
  }

  if (ReadonlyLocked() && (options.rights & fio::Rights::kWriteBytes) &&
      !vn->Supports(fuchsia_io::NodeProtocolKinds::kConnector)) {
    return ZX_ERR_ACCESS_DENIED;
  }

  if ((options.flags & (fio::OpenFlags::kPosixWritable | fio::OpenFlags::kPosixExecutable)) &&
      vn->Supports(fuchsia_io::NodeProtocolKinds::kDirectory)) {
    // This is such that POSIX open() can open a directory with O_RDONLY, and still get the
    // write/execute right if the parent directory connection has the write/execute right
    // respectively.  With the execute right in particular, the resulting connection may be passed
    // to fdio_get_vmo_exec() which requires the execute right. This transfers write and execute
    // from the parent, if present.
    fuchsia_io::Rights inheritable_rights;
    if (options.flags & fuchsia_io::OpenFlags::kPosixWritable) {
      inheritable_rights |= fuchsia_io::kWStarDir;
    }
    if (options.flags & fuchsia_io::OpenFlags::kPosixExecutable) {
      inheritable_rights |= fuchsia_io::kXStarDir;
    }
    options.rights |= connection_rights & inheritable_rights;
  }
  if (zx::result validated = vn->ValidateOptions(options); validated.is_error()) {
    return validated.error_value();
  }

  // |node_reference| requests that we don't actually open the underlying Vnode, but use the
  // connection as a reference to the Vnode.
  if (!(options.flags & fuchsia_io::OpenFlags::kNodeReference) && !vn_is_open) {
    if (zx_status_t status = OpenVnode(&vn); status != ZX_OK) {
      return status;
    }

    if (vn->IsRemote()) {
      // |OpenVnode| redirected us to a remote vnode; traverse across mount point.
      return OpenResult::Remote{.vnode = std::move(vn), .path = "."};
    }

    if (options.flags & fuchsia_io::OpenFlags::kTruncate) {
      if (zx_status_t status = vn->Truncate(0); status != ZX_OK) {
        vn->Close();
        return status;
      }
    }
  }

  return OpenResult::Ok{.vnode = std::move(vn), .options = options};
}

zx_status_t Vfs::Unlink(fbl::RefPtr<Vnode> vndir, std::string_view name, bool must_be_dir) {
  {
    std::lock_guard lock(vfs_lock_);
    if (ReadonlyLocked()) {
      return ZX_ERR_ACCESS_DENIED;
    }
    if (zx_status_t status = vndir->Unlink(name, must_be_dir); status != ZX_OK) {
      return status;
    }
  }
  return ZX_OK;
}

zx::result<std::tuple<fbl::RefPtr<Vnode>, /*vnode_is_open*/ bool>> Vfs::CreateOrLookup(
    fbl::RefPtr<fs::Vnode> vndir, std::string_view name, CreationMode mode,
    std::optional<CreationType> type, fio::Rights connection_rights) {
  // If the request requires we create an object, ensure the VFS isn't in read-only mode, and
  // that the connection has the correct rights.
  if (mode != CreationMode::kNever &&
      (ReadonlyLocked() || !(connection_rights & fuchsia_io::Rights::kModifyDirectory))) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  // If |name| points to this directory, just return |vndir|.
  if (name == ".") {
    if (mode == CreationMode::kAlways) {
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    return zx::ok(std::tuple{std::move(vndir), false});
  }
  // Try to create a new object if the request requires it.
  switch (mode) {
    case CreationMode::kAllowExisting:
    case CreationMode::kAlways: {
      ZX_DEBUG_ASSERT(type.has_value());
      // Try to create a new object and notify any watchers if one was added. Note that
      // |Vnode::Create()| ensures the returned object has already been opened on success.
      zx::result created = vndir->Create(name, *type);
      if (created.is_ok()) {
        vndir->Notify(name, fio::WatchEvent::kAdded);
        return zx::ok(std::tuple{*std::move(created), true});
      }
      // If |name| already exists in this directory, look it up if the request allows it.
      if (created.error_value() == ZX_ERR_ALREADY_EXISTS && mode == CreationMode::kAllowExisting) {
        break;
      }
      // If the filesystem doesn't support creating objects, we must still try to find an entry
      // matching |name| so we can return ZX_ERR_ALREADY_EXISTS. This is required for |open()| and
      // |mkdir()| to return EEXIST if an entry matching |name| already exists.
      //
      // *NOTE*: The POSIX specification states that |open()| and |mkdir()| should return EROFS if
      // the filesystem doesn't support creating new objects, and an existing one was not found.
      // Unfortunately there is no equivalent Zircon status that maps to that error code, nor can we
      // return ZX_ERR_NOT_SUPPORTED (ENOTSUP is not a valid error for these syscalls).
      //
      // For now, if we fail to find an existing object, we will return ZX_ERR_NOT_FOUND (ENOENT)
      // below. This doesn't cause an for most callers, as most only check for the EEXIST case,
      // which is common when specifying O_CREAT | O_EXCL.
      if (created.error_value() == ZX_ERR_NOT_SUPPORTED) {
        break;
      }
      return created.take_error();
    }
    case fs::CreationMode::kNever:
      break;
  }
  // We didn't create a new object, try to lookup an existing entry matching |name|.
  fbl::RefPtr<Vnode> vn;
  if (zx_status_t status = vndir->Lookup(name, &vn); status != ZX_OK) {
    return zx::error(status);
  }
  if (mode == CreationMode::kAlways) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }
  return zx::ok(std::tuple{std::move(vn), false});
}

zx::result<bool> Vfs::TrimName(std::string_view& name) {
  bool is_dir = false;
  while (!name.empty() && name.back() == '/') {
    is_dir = true;
    name.remove_suffix(1);
  }

  if (name.empty()) {
    // 'name' should not contain paths consisting of exclusively '/' characters.
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (name.length() > NAME_MAX) {
    // Name must be less than the maximum-expected length.
    return zx::error(ZX_ERR_BAD_PATH);
  }
  if (name.find('/') != std::string::npos) {
    // Name must not contain '/' characters after being trimmed.
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(is_dir);
}

zx_status_t Vfs::Readdir(Vnode* vn, VdirCookie* cookie, void* dirents, size_t len,
                         size_t* out_actual) {
  std::lock_guard lock(vfs_lock_);
  return vn->Readdir(cookie, dirents, len, out_actual);
}

void Vfs::SetReadonly(bool value) {
  std::lock_guard lock(vfs_lock_);
  readonly_ = value;
}

}  // namespace fs
