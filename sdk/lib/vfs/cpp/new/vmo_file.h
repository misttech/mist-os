// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_NEW_VMO_FILE_H_
#define LIB_VFS_CPP_NEW_VMO_FILE_H_

#include <lib/vfs/cpp/new/internal/node.h>
#include <lib/zx/vmo.h>
#include <zircon/availability.h>
#include <zircon/status.h>

#include <cstdint>
#include <vector>

namespace vfs {

// A file node backed by a range of bytes in a VMO.
//
// The file has a fixed size specified at creating time; it does not grow or shrink even when
// written into.
//
// This class is thread-safe.
class VmoFile final : public internal::Node {
 public:
  // Specifies the desired behavior of writes.
  enum class WriteMode : vfs_internal_write_mode_t {
    // The VmoFile is read only.
    READ_ONLY = VFS_INTERNAL_WRITE_MODE_READ_ONLY,
    // The VmoFile will be writable.
    WRITABLE = VFS_INTERNAL_WRITE_MODE_WRITABLE,
  };

  // Specifies the default behavior when a client asks for the file's underlying VMO, but does not
  // specify if a duplicate handle or copy-on-write clone is required.
  //
  // *NOTE*: This does not affect the behavior of requests that specify the required sharing mode.
  // Requests for a specific sharing mode will be fulfilled as requested.
  //
  // TODO(https://fxbug.dev/293936429): Introduce new constants for these enumerations that conform
  // to the Fuchsia C++ style guide, and deprecate the old ones.
  enum class DefaultSharingMode : vfs_internal_sharing_mode_t {
    // NOT_SUPPORTED will be returned, unless a sharing mode is specified in the request.
    NONE = VFS_INTERNAL_SHARING_MODE_NONE,

    // The VMO handle is duplicated for each client.
    //
    // This is appropriate when it is okay for clients to access the entire
    // contents of the VMO, possibly extending beyond the pages spanned by the
    // file.
    //
    // This mode is significantly more efficient than |CLONE_COW| and should be
    // preferred when file spans the whole VMO or when the VMO's entire content
    // is safe for clients to read.
    DUPLICATE = VFS_INTERNAL_SHARING_MODE_DUPLICATE,

    // The VMO range spanned by the file is cloned on demand, using
    // copy-on-write semantics to isolate modifications of clients which open
    // the file in a writable mode.
    //
    // This is appropriate when clients need to be restricted from accessing
    // portions of the VMO outside of the range of the file and when file
    // modifications by clients should not be visible to each other.
    CLONE_COW = VFS_INTERNAL_SHARING_MODE_COW,
  };

  // TODO(https://fxbug.dev/293936429): Deprecate these type aliases.
  using WriteOption = WriteMode;
  using Sharing = DefaultSharingMode;

  // Creates a file node backed by a VMO.
  VmoFile(zx::vmo vmo, size_t length, WriteMode write_option = WriteMode::READ_ONLY,
          DefaultSharingMode vmo_sharing = DefaultSharingMode::DUPLICATE)
      : VmoFile(vmo.release(), length, write_option, vmo_sharing) {}

  ~VmoFile() override = default;

  // TODO(https://fxbug.dev/293936429): Deprecate this method and share the VMO handle instead.
  zx_status_t ReadAt(uint64_t count, uint64_t offset, std::vector<uint8_t>* out_data) {
    if (count == 0u || offset >= length_) {
      return ZX_OK;
    }

    size_t remaining_length = length_ - offset;
    if (count > remaining_length) {
      count = remaining_length;
    }

    out_data->resize(count);
    return vmo_->read(out_data->data(), offset, count);
  }

 private:
  VmoFile(zx_handle_t vmo_handle, size_t length, WriteMode write_option,
          DefaultSharingMode vmo_sharing)
      : internal::Node(CreateVmoFile(vmo_handle, length, write_option, vmo_sharing)),
        vmo_(zx::unowned_vmo{vmo_handle}),
        length_(length) {}

  // The underlying node is responsible for closing `vmo_handle` when the node is destroyed.
  static inline vfs_internal_node_t* CreateVmoFile(zx_handle_t vmo_handle, size_t length,
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
  size_t length_;        // Required for `ReadAt()`.
};

}  // namespace vfs

#endif  // LIB_VFS_CPP_NEW_VMO_FILE_H_
