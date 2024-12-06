// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tree_builder.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <trace.h>
#include <zircon/errors.h>

#include <ktl/optional.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<zx_status_t> TreeBuilder::add_entry(const fbl::Vector<ktl::string_view>& path,
                                                ktl::unique_ptr<FsNodeOps> entry) {
  if (path.is_empty()) {
    return fit::error(ZX_ERR_BAD_PATH);
  }

  fbl::Vector<ktl::string_view> traversed;
  auto rest = path.begin();
  std::string_view name = *rest;

  if (name.empty()) {
    return fit::error(ZX_ERR_BAD_PATH);
  }

  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.size()), name.data());

  auto insert_into_map = [&](HashMap& entries, ktl::string_view name,
                             const fbl::Vector<ktl::string_view>& full_path,
                             fbl::Vector<ktl::string_view> _traversed) -> fit::result<zx_status_t> {
    auto [it, inserted] =
        entries.emplace(name, ktl::move(TreeBuilder(ktl::move(Leaf(ktl::move(entry))))));
    if (inserted) {
      return fit::ok();
    }
    return fit::error(ZX_ERR_BAD_PATH);
  };

  return add_path(path, ktl::move(traversed), name, rest, insert_into_map);
}

}  // namespace starnix
