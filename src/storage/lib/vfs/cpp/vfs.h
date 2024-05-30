// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_VFS_H_
#define SRC_STORAGE_LIB_VFS_CPP_VFS_H_

#include <lib/fdio/vfs.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <memory>
#include <mutex>
#include <set>
#include <string_view>
#include <utility>
#include <variant>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs {

class Vnode;

// A storage class for a vdircookie which is passed to Readdir. Common vnode implementations may use
// this struct as scratch space, or cast it to an alternative structure of the same size (or
// smaller).
struct VdirCookie {
  uint64_t n = 0;
  void* p = nullptr;
};

// The Vfs object contains global per-filesystem state, which may be valid across a collection of
// Vnodes. It dispatches requests to per-file/directory Vnode objects.
//
// This class can be used on a Fuchsia system or on the host computer where the compilation is done
// (the host builds of the filesystems are how system images are created). Normally Fuchsia builds
// will use the ManagedVfs subclass which handles the FIDL-to-vnode connections.
//
// The Vfs object must outlive the Vnodes which it serves. This class is thread-safe.
class Vfs {
 public:
  class OpenResult;
  class Open2Result;

  Vfs();
  virtual ~Vfs() = default;

  // Traverse the path to the target vnode, and create / open it using the underlying filesystem
  // functions (lookup, create, open).
  //
  // The return value will suggest the next action to take. Refer to the variants in |OpenResult|
  // for more information.
  OpenResult Open(fbl::RefPtr<Vnode> vn, std::string_view path, VnodeConnectionOptions options,
                  fuchsia_io::Rights connection_rights) __TA_EXCLUDES(vfs_lock_);

  // Traverse the path to the target node, and create or open it.
  zx::result<Open2Result> Open2(fbl::RefPtr<Vnode> vndir, std::string_view path,
                                const fuchsia_io::wire::ConnectionProtocols& protocols,
                                fuchsia_io::Rights connection_rights) __TA_EXCLUDES(vfs_lock_);

  // Implements Unlink for a pre-validated and trimmed name.
  virtual zx_status_t Unlink(fbl::RefPtr<Vnode> vn, std::string_view name, bool must_be_dir)
      __TA_EXCLUDES(vfs_lock_);

  // Calls readdir on the Vnode while holding the vfs_lock, preventing path modification operations
  // for the duration of the operation.
  zx_status_t Readdir(Vnode* vn, VdirCookie* cookie, void* dirents, size_t len, size_t* out_actual)
      __TA_EXCLUDES(vfs_lock_);

  // Sets whether this file system is read-only.
  void SetReadonly(bool value) __TA_EXCLUDES(vfs_lock_);

  // Query if this file system is read-only.
  bool IsReadonly() const __TA_EXCLUDES(vfs_lock_) {
    std::lock_guard lock(vfs_lock_);
    return readonly_;
  }

 protected:
  // Whether this file system is read-only.
  bool ReadonlyLocked() const __TA_REQUIRES(vfs_lock_) { return readonly_; }

  // Trim trailing slashes from name before sending it to internal filesystem functions. This also
  // validates whether the name has internal slashes and rejects them. Returns failure if the
  // resulting name is too long, empty, or contains slashes after trimming.
  //
  // Returns true iff name is suffixed with a trailing slash indicating an explicit reference to a
  // directory.
  static zx::result<bool> TrimName(std::string_view& name);

  // Create or lookup an entry with |name| inside of |vndir|. Returns a tuple of the resulting node,
  // and a boolean indicating if the returned vnode is open or not.
  zx::result<std::tuple<fbl::RefPtr<Vnode>, /*vnode_is_open*/ bool>> CreateOrLookup(
      fbl::RefPtr<fs::Vnode> vndir, std::string_view name, CreationMode mode,
      std::optional<CreationType> type, fuchsia_io::Rights connection_rights)
      __TA_REQUIRES(vfs_lock_);

  // A lock which should be used to protect lookup and walk operations
  mutable std::mutex vfs_lock_;

  // A separate lock to protected vnode registration. The vnodes will call into this class according
  // to their lifetimes, and many of these lifetimes are managed from within the VFS lock which can
  // result in reentrant locking. This lock should only be held for very short times when mutating
  // the registered node tracking information.
  mutable std::mutex live_nodes_lock_;

 private:
  bool readonly_ = false;
};

class Vfs::OpenResult {
 public:
  // When this variant is active, the indicated error occurred.
  using Error = zx_status_t;

  // When this variant is active, the path being opened contains a remote node. |path| is the
  // remaining portion of the path yet to be traversed. The caller should forward the remainder of
  // this open request to that vnode.
  //
  // Used only on Fuchsia.
  struct Remote {
    fbl::RefPtr<Vnode> vnode;
    std::string_view path;
  };

  // When this variant is active, |Open| has successfully reached a vnode under this filesystem.
  // |options| contains options to be used on the new connection, potentially adjusted for
  // posix-flag rights expansion.
  struct Ok {
    fbl::RefPtr<Vnode> vnode;
    VnodeConnectionOptions options;
  };

  // Forwards the constructor arguments into the underlying |std::variant|. This allows |OpenResult|
  // to be constructed directly from one of the variants, e.g.
  //
  //     OpenResult r = OpenResult::Error{ZX_ERR_ACCESS_DENIED};
  //
  template <typename T>
  OpenResult(T&& v) : variants_(std::forward<T>(v)) {}

  // Applies the |visitor| function to the variant payload. It simply forwards the visitor into the
  // underlying |std::variant|. Returns the return value of |visitor|. Refer to C++ documentation
  // for |std::visit|.
  template <class Visitor>
  constexpr auto visit(Visitor&& visitor) -> decltype(visitor(std::declval<zx_status_t>())) {
    return std::visit(std::forward<Visitor>(visitor), variants_);
  }

  Ok& ok() { return std::get<Ok>(variants_); }
  bool is_ok() const { return std::holds_alternative<Ok>(variants_); }

  Error& error() { return std::get<Error>(variants_); }
  bool is_error() const { return std::holds_alternative<Error>(variants_); }

  Remote& remote() { return std::get<Remote>(variants_); }
  bool is_remote() const { return std::holds_alternative<Remote>(variants_); }

 private:
  using Variants = std::variant<Error, Remote, Ok>;

  Variants variants_;
};

// Holds a (possibly opened) Vnode, ensuring that the open count is managed correctly.
class Vfs::Open2Result {
 public:
  // Cannot allow copy as this will result in the open count being incorrect.
  Open2Result(const Open2Result&) = delete;
  Open2Result& operator=(const Open2Result&) = delete;
  Open2Result(Open2Result&&) = default;
  Open2Result& operator=(Open2Result&&) = default;

  // Handles opening |vnode| if required based on the specified |protocol|. The |vnode| will be
  // closed when this object is destroyed, if required, unless |TakeVnode()| is called.
  static zx::result<Open2Result> OpenVnode(fbl::RefPtr<fs::Vnode> vnode, VnodeProtocol protocol) {
    // We don't open the node for node reference connections.
    if (protocol == VnodeProtocol::kNode) {
      return Open2Result::Local(std::move(vnode), protocol);
    }
    if (zx_status_t status = fs::OpenVnode(&vnode); status != ZX_OK) {
      return zx::error(status);
    }
    if (vnode->IsRemote()) {
      // Opening the node redirected us to a remote, forward the request to it.
      return Open2Result::Remote(std::move(vnode), ".");
    }
    return Open2Result::Local(std::move(vnode), protocol);
  }

  // Creates a new |Open2Result| from a remote |vnode|. This object keeps an unowned copy of |path|,
  // so the underlying string must outlive this object.
  static zx::result<Open2Result> Remote(fbl::RefPtr<fs::Vnode> vnode, std::string_view path) {
    ZX_DEBUG_ASSERT(vnode->IsRemote());
    return zx::ok(Open2Result(std::move(vnode), path));
  }

  // Creates a new |Open2Result| from a local node. |vnode| is assumed to already have been opened,
  // and unless |TakeVnode()| is called, will be closed when this object is destroyed.
  static zx::result<Open2Result> Local(fbl::RefPtr<fs::Vnode> vnode, VnodeProtocol protocol) {
    ZX_DEBUG_ASSERT(!vnode->IsRemote());
    return zx::ok(Open2Result(std::move(vnode), protocol));
  }

  // Ensure we roll back the vnode open count when required. There are only two cases where we do
  // not open a vnode: 1) remote nodes, and 2) node-reference connections
  ~Open2Result() {
    if (vnode_ && !vnode_->IsRemote() && protocol_ && *protocol_ != VnodeProtocol::kNode) {
      vnode_->Close();
    }
  }

  // Take the vnode out of this object. The caller is responsible for closing the node if required.
  fbl::RefPtr<fs::Vnode> TakeVnode() { return std::exchange(vnode_, nullptr); }
  const fbl::RefPtr<fs::Vnode>& vnode() const { return vnode_; }

  // The protocol which was negotiated for this node if we resolved it locally. The vnode must not
  // be a remote, as the remote server is responsible for performing protocol negotiation.
  fs::VnodeProtocol protocol() const {
    ZX_DEBUG_ASSERT(!vnode_->IsRemote());
    return *protocol_;
  }

  // Remainder of the path to forward if this is a remote node.
  std::string_view path() const { return path_; }

 private:
  Open2Result() = delete;
  Open2Result(fbl::RefPtr<fs::Vnode> vnode, fs::VnodeProtocol protocol)
      : vnode_(std::move(vnode)), protocol_(protocol) {}
  Open2Result(fbl::RefPtr<fs::Vnode> vnode, std::string_view path)
      : vnode_(std::move(vnode)), path_(path) {}

  fbl::RefPtr<fs::Vnode> vnode_;
  std::optional<fs::VnodeProtocol> protocol_;
  std::string_view path_;
};

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_VFS_H_
