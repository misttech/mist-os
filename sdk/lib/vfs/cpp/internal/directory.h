
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_INTERNAL_DIRECTORY_H_
#define LIB_VFS_CPP_INTERNAL_DIRECTORY_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/vfs/cpp/internal/node.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <stdint.h>

#include <optional>
#include <string>
#include <string_view>

namespace vfs {

namespace internal {

// A directory object in a file system.
//
// Implements the |fuchsia.io.Directory| interface. Incoming connections are
// owned by this object and will be destroyed when this object is destroyed.
//
// Subclass to implement specific directory semantics.
//
// See also:
//
//  * File, which represents file objects.
class Directory : public Node {
 public:
  Directory();
  ~Directory() override;

  // |Node| implementation
  zx_status_t Lookup(const std::string& name, Node** out_node) const override;

  void Describe(fuchsia::io::NodeInfoDeprecated* out_info) override;

  // Enumerates Directory
  //
  // |offset| will start with 0 and then implementation can set offset as it
  // pleases.
  //
  // Returns |ZX_OK| if able to read at least one dentry else returns
  // |ZX_ERR_INVALID_ARGS| with |out_actual| as 0 and |out_offset| as |offset|.
  virtual zx_status_t Readdir(uint64_t offset, void* data, uint64_t len, uint64_t* out_offset,
                              uint64_t* out_actual) = 0;

  // Parses path and opens correct node.
  //
  // Called from |fuchsia.io.Directory#Open|.
  void Open(fuchsia::io::OpenFlags open_flags, fuchsia::io::OpenFlags parent_flags,
            fuchsia::io::ModeType mode, std::string_view path, zx::channel request,
            async_dispatcher_t* dispatcher);

  // Validates passed path
  //
  // Returns |false| if |path| is longer than |NAME_MAX| or if
  // |path| starts with ".." or "/".
  // Returns |true| on valid path.
  static bool ValidatePath(std::string_view path);

  struct WalkPathResult {
    std::optional<std::string_view> current_node_name;
    std::string_view remaining_path;
  };

  // Walks provided path to find the first node name in |path| and returns
  // remaining path and current node name if not self.
  //
  // Calls |ValidatePath| and returns |ZX_ERR_INVALID_ARGS| on failure.
  //
  // Supports paths like "a/./b//."
  // Supports repetitive "/"
  // Doesn't support "a/../a/b"
  //
  // eg:
  // path ="a/b/c/d", |WalkPathResult::remaining_path| would be "b/c/d"
  // path =".", |WalkPathResult::remaining_path| would be empty
  // path ="./", |WalkPathResult::remaining_path| would be empty
  // path ="a/b/", |WalkPathResult::remaining_path| would be "b/"
  static zx::result<WalkPathResult> WalkPath(std::string_view path);

  // |Node| implementation
  zx_status_t GetAttr(fuchsia::io::NodeAttributes* out_attributes) const override;

  bool IsDirectory() const override;

 protected:
  // |Node| implementations
  zx_status_t CreateConnection(fuchsia::io::OpenFlags flags,
                               std::unique_ptr<Connection>* connection) override;

  fuchsia::io::OpenFlags GetAllowedFlags() const override;
  fuchsia::io::OpenFlags GetProhibitiveFlags() const override;

  struct LookupPathResult {
    Node& node;
    std::string_view remaining_path;
    bool is_dir;
  };

  // Walks |path| until the node corresponding to |path| is found, or a remote
  // filesystem was encountered during traversal. In the latter case, this
  // function will return an intermediate node, on which |IsRemote| returns
  // true, and will return the remaining path.
  //
  // For example: if path is "a/b/c/d/f/g" and c is a remote node, it will return
  // {
  //   node: c,
  //   remaining_path: "d/f/g",
  //   is_dir: false,
  // }
  //
  // |is_dir| is true if path has '/' or '/.' at the end.
  //
  // Calls |WalkPath| in loop and returns status on error. Returns
  // |ZX_ERR_NOT_DIR| if an intermediate component of |path| is not a directory.
  zx::result<LookupPathResult> LookupPath(std::string_view path);
};

}  // namespace internal
}  // namespace vfs

#endif  // LIB_VFS_CPP_INTERNAL_DIRECTORY_H_
