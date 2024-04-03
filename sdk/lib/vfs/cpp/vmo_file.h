// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_VMO_FILE_H_
#define LIB_VFS_CPP_VMO_FILE_H_

#include <lib/vfs/cpp/node.h>
#include <lib/zx/vmo.h>
#include <zircon/availability.h>
#include <zircon/status.h>

#include <cstdint>

namespace vfs {

// A file node backed by a range of bytes in a VMO.
//
// The file has a fixed size specified at creating time; it does not grow or shrink even when
// written into.
//
// This class is thread-safe.
class VmoFile final : public Node {
 public:
  // TODO(https://fxbug.dev/311176363): Remove deprecated enum constants and type aliases below.

  // Specifies the desired behavior of writes.
  enum class WriteMode : vfs_internal_write_mode_t {
    // The VmoFile is read only.
    kReadOnly = VFS_INTERNAL_WRITE_MODE_READ_ONLY,
    // The VmoFile will be writable.
    kWritable = VFS_INTERNAL_WRITE_MODE_WRITABLE,
    READ_ONLY ZX_REMOVED_SINCE(1, 19, 20, "Use kReadOnly instead.") = kReadOnly,
    WRITABLE ZX_REMOVED_SINCE(1, 19, 20, "Use kWritable instead.") = kWritable,
  };

  // Specifies the default behavior when a client asks for the file's underlying VMO, but does not
  // specify if a duplicate handle or copy-on-write clone is required.
  //
  // *NOTE*: This does not affect the behavior of requests that specify the required sharing mode.
  // Requests for a specific sharing mode will be fulfilled as requested.
  enum class DefaultSharingMode : vfs_internal_sharing_mode_t {
    // Will return `ZX_ERR_NOT_SUPPORTED` if a sharing mode was not specified in the request.
    kNone = VFS_INTERNAL_SHARING_MODE_NONE,

    // The VMO handle is duplicated for each client.
    //
    // This is appropriate when it is okay for clients to access the entire
    // contents of the VMO, possibly extending beyond the pages spanned by the
    // file.
    //
    // This mode is significantly more efficient than |CLONE_COW| and should be
    // preferred when file spans the whole VMO or when the VMO's entire content
    // is safe for clients to read.
    kDuplicate = VFS_INTERNAL_SHARING_MODE_DUPLICATE,

    // The VMO range spanned by the file is cloned on demand, using
    // copy-on-write semantics to isolate modifications of clients which open
    // the file in a writable mode.
    //
    // This is appropriate when clients need to be restricted from accessing
    // portions of the VMO outside of the range of the file and when file
    // modifications by clients should not be visible to each other.
    kCloneCow = VFS_INTERNAL_SHARING_MODE_COW,

    NONE ZX_REMOVED_SINCE(1, 19, 20, "Use kNone instead.") = kNone,
    DUPLICATE ZX_REMOVED_SINCE(1, 19, 20, "Use kDuplicate instead.") = kDuplicate,
    CLONE_COW ZX_REMOVED_SINCE(1, 19, 20, "Use kCloneCow instead.") = kCloneCow,
  };

  using WriteOption ZX_REMOVED_SINCE(1, 19, 20, "Use WriteMode instead.") = WriteMode;
  using Sharing ZX_REMOVED_SINCE(1, 19, 20, "Use DefaultSharingMode instead.") = DefaultSharingMode;

  // Creates a file node backed by a VMO.
  VmoFile(zx::vmo vmo, size_t length, WriteMode write_option = WriteMode::kReadOnly,
          DefaultSharingMode vmo_sharing = DefaultSharingMode::kDuplicate)
      : VmoFile(vmo.release(), length, write_option, vmo_sharing) {}

  using Node::Serve;

  // Returns a borrowed handle to the VMO backing this file.
  zx::unowned_vmo vmo() const { return vmo_->borrow(); }

 private:
  VmoFile(zx_handle_t vmo_handle, size_t length, WriteMode write_option,
          DefaultSharingMode vmo_sharing)
      : Node(CreateVmoFile(vmo_handle, length, write_option, vmo_sharing)),
        vmo_(zx::unowned_vmo{vmo_handle}) {}

  // The underlying node is responsible for closing `vmo_handle` when the node is destroyed.
  static vfs_internal_node_t* CreateVmoFile(zx_handle_t vmo_handle, size_t length,
                                            WriteMode write_option,
                                            DefaultSharingMode vmo_sharing) {
    vfs_internal_node_t* vmo_file;
    ZX_ASSERT(vfs_internal_vmo_file_create(vmo_handle, static_cast<uint64_t>(length),
                                           static_cast<vfs_internal_write_mode_t>(write_option),
                                           static_cast<vfs_internal_sharing_mode_t>(vmo_sharing),
                                           &vmo_file) == ZX_OK);
    return vmo_file;
  }

  zx::unowned_vmo vmo_;  // Cannot outlive underlying node.
};

}  // namespace vfs

#endif  // LIB_VFS_CPP_VMO_FILE_H_
