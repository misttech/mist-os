// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_NEW_LAZY_DIR_H_
#define LIB_VFS_CPP_NEW_LAZY_DIR_H_

#include <lib/vfs/cpp/new/internal/node.h>
#include <zircon/availability.h>
#include <zircon/types.h>

#include <string>

// TODO(https://fxbug.dev/293936429): Implement this or migrate out of tree callers.

namespace vfs {

// A |LazyDir| a base class for directories that dynamically update their
// contents on each operation.  Clients should derive from this class
// and implement GetContents and GetFile for their use case.
//
// This class is thread-hostile, as are the |Nodes| it manages.
//
//  # Simple usage
//
// Instances of this class should be owned and managed on the same thread
// that services their connections.
//
// # Advanced usage
//
// You can use a background thread to service connections provided: (a) the
// contents of the directory are configured prior to starting to service
// connections, (b) all modifications to the directory occur while the
// async_dispatcher_t for the background thread is stopped or suspended, and
// (c) async_dispatcher_t for the background thread is stopped or suspended
// prior to destroying the directory.
//
// TODO(https://fxbug.dev/309685624): Remove LazyDir once all out-of-tree users have been migrated.
class LazyDir : public vfs::internal::Node {
 public:
  LazyDir() : Node(nullptr) { ZX_PANIC("TODO(https://fxbug.dev/293936429)"); }
  ~LazyDir() override = default;

  // Structure storing a single entry in the directory.
  struct LazyEntry {
    // Should be more than or equal to |GetStartingId()|, must remain stable
    // across calls.
    uint64_t id;
    std::string name;
    uint32_t type;

    bool operator<(const LazyEntry& rhs) const;
  };
  using LazyEntryVector = std::vector<LazyEntry>;

 protected:
  // Get the contents of the directory in an output vector.
  virtual void GetContents(LazyEntryVector* out_vector) const = 0;

  // Get the reference to a single file. The id and name of the entry as
  // returned from GetContents are passed in to assist locating the file.
  virtual zx_status_t GetFile(Node** out_node, uint64_t id, std::string name) const = 0;

  // Ids returned by |GetContent| should be more than or equal to id returned by
  // this function.
  uint64_t GetStartingId() const { return kDotId; }

 private:
  static constexpr uint64_t kDotId = 1u;
} ZX_DEPRECATED_SINCE(1, 16, "Use PseudoDir or RemoteDir instead.");

}  // namespace vfs

#endif  // LIB_VFS_CPP_NEW_LAZY_DIR_H_
