
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/io/cpp/fidl.h>
#include <lib/fdio/vfs.h>
#include <lib/vfs/cpp/flags.h>
#include <lib/vfs/cpp/internal/directory.h>
#include <lib/vfs/cpp/internal/directory_connection.h>
#include <zircon/errors.h>

#include <optional>
#include <string_view>

#include "lib/stdcompat/string_view.h"

namespace vfs {
namespace internal {

Directory::Directory() = default;

Directory::~Directory() = default;

void Directory::Describe(fuchsia::io::NodeInfoDeprecated* out_info) {
  out_info->set_directory(fuchsia::io::DirectoryObject());
}

zx_status_t Directory::Lookup(const std::string& name, Node** out_node) const {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Directory::CreateConnection(fuchsia::io::OpenFlags flags,
                                        std::unique_ptr<Connection>* connection) {
  *connection = std::make_unique<internal::DirectoryConnection>(flags, this);
  return ZX_OK;
}

bool Directory::ValidatePath(std::string_view path) {
  if (path.length() > NAME_MAX) {
    return false;
  }
  if (path == "..") {
    return false;
  }
  if (cpp20::starts_with(path, '/')) {
    return false;
  }
  if (cpp20::starts_with(path, "../")) {
    return false;
  }
  return true;
}

bool Directory::IsDirectory() const { return true; }

fuchsia::io::OpenFlags Directory::GetAllowedFlags() const {
  return fuchsia::io::OpenFlags::DIRECTORY | fuchsia::io::OpenFlags::RIGHT_READABLE |
         fuchsia::io::OpenFlags::RIGHT_WRITABLE | fuchsia::io::OpenFlags::RIGHT_EXECUTABLE;
}

fuchsia::io::OpenFlags Directory::GetProhibitiveFlags() const {
  return fuchsia::io::OpenFlags::CREATE | fuchsia::io::OpenFlags::CREATE_IF_ABSENT |
         fuchsia::io::OpenFlags::TRUNCATE | fuchsia::io::OpenFlags::APPEND;
}

zx_status_t Directory::GetAttr(fuchsia::io::NodeAttributes* out_attributes) const {
  out_attributes->mode = fuchsia::io::MODE_TYPE_DIRECTORY | V_IRUSR;
  out_attributes->id = fuchsia::io::INO_UNKNOWN;
  out_attributes->content_size = 0;
  out_attributes->storage_size = 0;
  out_attributes->link_count = 1;
  out_attributes->creation_time = 0;
  out_attributes->modification_time = 0;
  return ZX_OK;
}

zx::result<Directory::WalkPathResult> Directory::WalkPath(std::string_view path) {
  if (!ValidatePath(path)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // remove any "./", ".//", etc
  while (cpp20::starts_with(path, "./")) {
    path.remove_prefix(2);
    while (cpp20::starts_with(path, '/')) {
      path.remove_prefix(1);
    }
  }

  if (path.empty() || path == ".") {
    return zx::ok(Directory::WalkPathResult{
        .remaining_path = path,
    });
  }

  // Lookup node
  if (size_t index = path.find('/'); index != std::string::npos) {
    std::string_view current_node_name = path.substr(0, index);
    path.remove_prefix(index + 1);
    while (cpp20::starts_with(path, '/')) {
      path.remove_prefix(1);
    }
    return zx::ok(Directory::WalkPathResult{
        .current_node_name = current_node_name,
        .remaining_path = path,
    });
  }

  return zx::ok(Directory::WalkPathResult{
      .current_node_name = path,
  });
}

zx::result<Directory::LookupPathResult> Directory::LookupPath(std::string_view path) {
  bool is_dir = path.empty() || cpp20::ends_with(path, '/');
  Node* current_node = this;
  do {
    zx::result result = WalkPath(path);
    if (result.is_error()) {
      return result.take_error();
    }
    Directory::WalkPathResult& walk_path_result = result.value();
    if (!walk_path_result.current_node_name.has_value()) {
      return zx::ok(Directory::LookupPathResult{
          .node = *current_node,
          .remaining_path = walk_path_result.remaining_path,
          .is_dir = true,
      });
    }
    path = walk_path_result.remaining_path;
    if (zx_status_t status = current_node->Lookup(
            std::string(walk_path_result.current_node_name.value()), &current_node);
        status != ZX_OK) {
      return zx::error(status);
    }
    if (current_node->IsRemote()) {
      break;
    }
  } while (!path.empty());

  return zx::ok(LookupPathResult{
      .node = *current_node,
      .remaining_path = path,
      .is_dir = is_dir,
  });
}

void Directory::Open(fuchsia::io::OpenFlags open_flags, fuchsia::io::OpenFlags parent_flags,
                     fuchsia::io::ModeType mode, std::string_view path, zx::channel request,
                     async_dispatcher_t* dispatcher) {
  if (!Flags::InputPrecondition(open_flags)) {
    Node::SendOnOpenEventOnError(open_flags, std::move(request), ZX_ERR_INVALID_ARGS);
    return;
  }
  if (Flags::ShouldCloneWithSameRights(open_flags)) {
    Node::SendOnOpenEventOnError(open_flags, std::move(request), ZX_ERR_INVALID_ARGS);
    return;
  }
  if (!Flags::StricterOrSameRights(open_flags, parent_flags)) {
    Node::SendOnOpenEventOnError(open_flags, std::move(request), ZX_ERR_ACCESS_DENIED);
    return;
  }

  if (path.empty() || path.length() > PATH_MAX - 1) {
    Node::SendOnOpenEventOnError(open_flags, std::move(request), ZX_ERR_BAD_PATH);
    return;
  }
  // OPEN_FLAG_NOT_DIRECTORY implies can't open "." because we're a directory and can't open a
  // subdirectory indicated by a trailing slash.
  if (Flags::IsNotDirectory(open_flags) && (path == "." || path.back() == '/')) {
    Node::SendOnOpenEventOnError(open_flags, std::move(request), ZX_ERR_INVALID_ARGS);
    return;
  }

  // TODO(fxbug.dev/119413): Path handling does not conform to in-tree behavior.
  zx::result result = LookupPath(path);
  if (result.is_error()) {
    return SendOnOpenEventOnError(open_flags, std::move(request), result.error_value());
  }
  LookupPathResult& look_path_result = result.value();

  // The POSIX compatibility flags allow the child dir connection to inherit
  // write/execute rights from its immediate parent. We remove the right inheritance
  // anywhere the parent node does not have the relevant right.
  if (Flags::IsPosixWritable(open_flags) && !Flags::IsWritable(parent_flags)) {
    open_flags &= ~fuchsia::io::OpenFlags::POSIX_WRITABLE;
  }
  if (Flags::IsPosixExecutable(open_flags) && !Flags::IsExecutable(parent_flags)) {
    open_flags &= ~fuchsia::io::OpenFlags::POSIX_EXECUTABLE;
  }

  if (look_path_result.node.IsRemote() && !look_path_result.remaining_path.empty()) {
    look_path_result.node.OpenRemote(open_flags, mode, look_path_result.remaining_path,
                                     fidl::InterfaceRequest<fuchsia::io::Node>(std::move(request)));
    return;
  }

  // Perform POSIX rights expansion if required.
  if (look_path_result.node.IsDirectory()) {
    if (Flags::IsPosixWritable(open_flags))
      open_flags |= (parent_flags & fuchsia::io::OpenFlags::RIGHT_WRITABLE);
    if (Flags::IsPosixExecutable(open_flags))
      open_flags |= (parent_flags & fuchsia::io::OpenFlags::RIGHT_EXECUTABLE);
    // Strip flags now that rights have been expanded.
    open_flags &=
        ~(fuchsia::io::OpenFlags::POSIX_WRITABLE | fuchsia::io::OpenFlags::POSIX_EXECUTABLE);
  }

  if (look_path_result.is_dir) {
    open_flags |= fuchsia::io::OpenFlags::DIRECTORY;
  }
  look_path_result.node.Serve(open_flags, std::move(request), dispatcher);
}

}  // namespace internal
}  // namespace vfs
