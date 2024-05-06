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

  Vfs();
  virtual ~Vfs() = default;

  // Traverse the path to the target vnode, and create / open it using the underlying filesystem
  // functions (lookup, create, open).
  //
  // The return value will suggest the next action to take. Refer to the variants in |OpenResult|
  // for more information.
  OpenResult Open(fbl::RefPtr<Vnode> vn, std::string_view path, VnodeConnectionOptions options,
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

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_VFS_H_
