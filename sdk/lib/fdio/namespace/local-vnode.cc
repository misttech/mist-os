// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "local-vnode.h"

#include <lib/zx/channel.h>
#include <lib/zxio/ops.h>
#include <lib/zxio/zxio.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <fbl/string_buffer.h>

#include "sdk/lib/fdio/internal.h"
#include "sdk/lib/fdio/zxio.h"

namespace fdio_internal {

void LocalVnode::Intermediate::AddEntry(fbl::RefPtr<LocalVnode> vn) {
  // |fdio_namespace| already checked that the entry does not exist.
  ZX_DEBUG_ASSERT(entries_by_name_.find(vn->Name()) == entries_by_name_.end());

  auto entry = std::make_unique<Entry>(next_node_id_, std::move(vn));
  entries_by_name_.insert(entry.get());
  entries_by_id_.insert(std::move(entry));
  next_node_id_++;
}

void LocalVnode::Intermediate::RemoveEntry(LocalVnode* vn) {
  auto it = entries_by_name_.find(vn->Name());
  if (it != entries_by_name_.end() && it->node().get() == vn) {
    auto id = it->id();
    entries_by_name_.erase(it);
    entries_by_id_.erase(id);
  }
}

fbl::RefPtr<LocalVnode> LocalVnode::Intermediate::Lookup(std::string_view name) const {
  auto it = entries_by_name_.find(fbl::String{name});
  if (it != entries_by_name_.end()) {
    return it->node();
  }
  return nullptr;
}

LocalVnode::~LocalVnode() {
  std::visit(fdio::overloaded{
                 [](LocalVnode::Local& c) {},
                 [](LocalVnode::Intermediate& c) {},
                 [](LocalVnode::Remote& s) {
                   // Close the channel underlying the remote connection without making a Close
                   // call to preserve previous behavior.
                   zx::channel remote_channel;
                   zxio_release(s.Connection(), remote_channel.reset_and_get_address());
                 },
             },
             node_type_);
}

LocalVnode::Intermediate::~Intermediate() {
  for (auto& entry : entries_by_id_) {
    entry.node()->parent_ = nullptr;
  }
  entries_by_name_.clear();
  entries_by_id_.clear();
}

LocalVnode::Local::Local(fdio_open_local_func_t on_open, void* context)
    : on_open_(on_open), context_(context) {}

zx::result<fdio_ptr> LocalVnode::Local::Open() {
  if (on_open_ == nullptr) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  fdio_ptr io = fbl::MakeRefCounted<zxio>();
  if (io == nullptr) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  zxio_ops_t const* ops = nullptr;
  zx_status_t status = on_open_(&io->zxio_storage(), context_, &ops);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  zxio_init(&io->zxio_storage().io, ops);
  return zx::ok(io);
}

void LocalVnode::UnlinkFromParent() {
  if (parent_ != nullptr) {
    parent_->RemoveEntry(this);
    parent_ = nullptr;
  }
}

zx_status_t LocalVnode::EnumerateInternal(PathBuffer* path, const EnumerateCallback& func) const {
  const size_t original_length = path->length();

  // Add this current node to the path, and enumerate it if it has a remote
  // object.
  path->Append(Name().data(), Name().length());

  std::visit(fdio::overloaded{
                 [](const LocalVnode::Local& c) {
                   // Nothing to do as the node has no children and is not a
                   // remote node.
                 },
                 [&path, &func, this](const LocalVnode::Intermediate& c) {
                   // If we added a node with children, add a separator and enumerate all the
                   // children.
                   if (!Name().empty()) {
                     path->Append('/');
                   }

                   c.ForAllEntries([&path, &func](const LocalVnode& child) {
                     return child.EnumerateInternal(path, func);
                   });
                 },
                 [&path, &func](const LocalVnode::Remote& s) {
                   // If we added a remote node, call the enumeration function on the remote node.
                   func(std::string_view(path->data(), path->length()), s.Connection());
                 },
             },
             node_type_);

  // To re-use the same prefix buffer, restore the original buffer length
  // after enumeration has completed.
  path->Resize(original_length);
  return ZX_OK;
}

zx_status_t LocalVnode::EnumerateRemotes(const EnumerateCallback& func) const {
  PathBuffer path;
  path.Append('/');
  return EnumerateInternal(&path, func);
}

zx_status_t LocalVnode::Readdir(uint64_t* last_seen, fbl::RefPtr<LocalVnode>* out_vnode) const {
  return std::visit(fdio::overloaded{
                        [&](const LocalVnode::Local& c) {
                          // Calling readdir on a Local node is invalid.
                          return ZX_ERR_NOT_DIR;
                        },
                        [&](const LocalVnode::Intermediate& c) {
                          for (auto it = c.GetEntriesById().lower_bound(*last_seen);
                               it != c.GetEntriesById().end(); ++it) {
                            if (it->id() <= *last_seen) {
                              continue;
                            }
                            *last_seen = it->id();
                            *out_vnode = it->node();
                            return ZX_OK;
                          }
                          *out_vnode = nullptr;
                          return ZX_OK;
                        },
                        [](const LocalVnode::Remote& s) {
                          // If we've called Readdir on a Remote node, the path
                          // was misconfigured.
                          return ZX_ERR_BAD_PATH;
                        },
                    },
                    node_type_);
}

template <typename Fn>
zx_status_t LocalVnode::Intermediate::ForAllEntries(Fn fn) const {
  for (const Entry& entry : entries_by_id_) {
    zx_status_t status = fn(*entry.node());
    if (status != ZX_OK) {
      return status;
    }
  }
  return ZX_OK;
}

}  // namespace fdio_internal
