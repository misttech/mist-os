// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/bind/bind_node_set.h"

namespace driver_manager {

void BindNodeSet::StartNextBindProcess() {
  if (is_bind_ongoing_) {
    CompleteOngoingBind();
  }

  new_orphaned_nodes_ = orphaned_nodes_;
  is_bind_ongoing_ = true;
  NotifyBindState();
}

void BindNodeSet::EndBindProcess() {
  ZX_ASSERT(is_bind_ongoing_);
  CompleteOngoingBind();
  is_bind_ongoing_ = false;
  NotifyBindState();
}

void BindNodeSet::CompleteOngoingBind() {
  ZX_ASSERT(is_bind_ongoing_);
  orphaned_nodes_ = std::move(new_orphaned_nodes_);

  for (auto& [path, node_weak] : new_multibind_nodes_) {
    multibind_nodes_[path] = node_weak;
  }
  new_multibind_nodes_ = {};
}

void BindNodeSet::NotifyBindState() {
  if (on_bind_state_changed_) {
    on_bind_state_changed_();
  }
}

void BindNodeSet::AddOrphanedNode(Node& node) {
  std::string moniker = node.MakeComponentMoniker();
  ZX_ASSERT(!MultibindContains(moniker));
  if (is_bind_ongoing_) {
    new_orphaned_nodes_[moniker] = node.weak_from_this();
    return;
  }
  orphaned_nodes_[moniker] = node.weak_from_this();
}

void BindNodeSet::RemoveOrphanedNode(std::string node_moniker) {
  if (is_bind_ongoing_) {
    new_orphaned_nodes_.erase(node_moniker);
    return;
  }
  orphaned_nodes_.erase(node_moniker);
}

void BindNodeSet::AddOrMoveMultibindNode(Node& node) {
  RemoveOrphanedNode(node.MakeComponentMoniker());
  if (is_bind_ongoing_) {
    new_multibind_nodes_[node.MakeComponentMoniker()] = node.weak_from_this();
    return;
  }
  multibind_nodes_[node.MakeComponentMoniker()] = node.weak_from_this();
}

bool BindNodeSet::MultibindContains(std::string node_moniker) const {
  return multibind_nodes_.find(node_moniker) != multibind_nodes_.end() ||
         new_multibind_nodes_.find(node_moniker) != new_multibind_nodes_.end();
}

}  // namespace driver_manager
