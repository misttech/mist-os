// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/device/kobject.h"

#include <lib/mistos/starnix/kernel/fs/sysfs/kobject_directory.h>

namespace starnix {

KObjectHandle KObject::new_root(const FsString& name) {
  return new_root_with_dir<FsNodeOps>(name, KObjectDirectory::New);
}

ktl::optional<KObjectHandle> KObject::parent() const {
  if (parent_.has_value()) {
    return ktl::optional<KObjectHandle>{parent_.value().Lock()};
  }
  return ktl::nullopt;
}

ktl::unique_ptr<FsNodeOps> KObject::ops() {
  return create_fs_node_ops_(util::WeakPtr<KObject>(this));
}

FsString KObject::path() {
  auto current = ktl::optional<KObjectHandle>{fbl::RefPtr(this)};
  auto path = PathBuilder::New();

  while (current) {
    path.prepend_element((*current)->name_);
    current = (*current)->parent();
  }

  return path.build_relative();
}

FsString KObject::path_to_root() const {
  auto parent = this->parent();
  auto path = PathBuilder::New();

  while (parent) {
    path.prepend_element(FsString(".."));
    parent = (*parent)->parent();
  }

  return path.build_relative();
}

bool KObject::has_child(const FsString& name) const { return get_child(name).has_value(); }

ktl::optional<KObjectHandle> KObject::get_child(const FsString& name) const {
  auto guard = children_.Lock();
  auto it = guard->find(name);
  if (it != guard->end()) {
    return it->second;
  }
  return ktl::nullopt;
}

void KObject::insert_child(KObjectHandle child) {
  auto guard = children_.Lock();
  guard->try_emplace(child->name(), child);
}

void KObject::insert_child_with_name(const FsString& name, KObjectHandle child) {
  auto guard = children_.Lock();
  guard->try_emplace(name, child);
}

fbl::Vector<FsString> KObject::get_children_names() const {
  auto guard = children_.Lock();
  fbl::Vector<FsString> names;
  fbl::AllocChecker ac;

  for (const auto& pair : *guard) {
    names.push_back(pair.first, &ac);
    ZX_ASSERT(ac.check());
  }

  return names;
}

fbl::Vector<KObjectHandle> KObject::get_children_kobjects() const {
  auto guard = children_.Lock();
  fbl::Vector<KObjectHandle> kobjects;
  fbl::AllocChecker ac;

  for (const auto& pair : *guard) {
    kobjects.push_back(pair.second, &ac);
    ZX_ASSERT(ac.check());
  }

  return kobjects;
}

ktl::optional<ktl::pair<FsString, KObjectHandle>> KObject::remove_child(const FsString& name) {
  auto guard = children_.Lock();
  auto it = guard->find(name);
  if (it == guard->end()) {
    return ktl::nullopt;
  }

  auto result = ktl::pair(it->first, it->second);
  guard->erase(it);
  return result;
}

void KObject::remove() {
  if (auto parent = this->parent()) {
    (*parent)->remove_child(this->name());
  }
}
}  // namespace starnix
