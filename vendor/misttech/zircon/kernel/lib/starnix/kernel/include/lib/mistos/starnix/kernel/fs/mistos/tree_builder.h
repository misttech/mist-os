// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_TREE_BUILDER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_TREE_BUILDER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/kernel/vfs/simple_directory.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/util/allocator.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <map>

#include <fbl/alloc_checker.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

namespace starnix {

using FileMode = starnix_uapi::FileMode;
using FsCred = starnix_uapi::FsCred;

class TreeBuilder;
class SimpleDirectory;

/*using HashMap = std::unordered_map<ktl::string_view, TreeBuilder, std::hash<ktl::string_view>,
                                   std::equal_to<ktl::string_view>,
                                   Allocator<ktl::pair<const ktl::string_view, TreeBuilder>>>;*/

using HashMap = std::map<FsStr, TreeBuilder, std::less<const FsStr>,
                         util::Allocator<ktl::pair<const FsStr, TreeBuilder>>>;

struct Directory {
  HashMap entries_;
};

struct Leaf {
  ktl::unique_ptr<FsNodeOps> entry_;
};

class TreeBuilder {
 public:
  /// impl TreeBuilder

  /// Constructs an empty builder.  It is always an empty [`crate::directory::immutable::Simple`]
  /// directory.
  static TreeBuilder empty_dir() { return TreeBuilder(ktl::move(Directory())); }

  // Adds a [`DirectoryEntry`] at the specified path.  It can be either a file or a directory.
  /// In case it is a directory, this builder cannot add new child nodes inside of the added
  /// directory.  Any `entry` is treated as an opaque "leaf" as far as the builder is concerned.
  fit::result<zx_status_t> add_entry(const fbl::Vector<ktl::string_view>& path,
                                     ktl::unique_ptr<FsNodeOps> entry);

  template <typename InserterFn>
  fit::result<zx_status_t> add_path(const fbl::Vector<ktl::string_view>& full_path,
                                    fbl::Vector<ktl::string_view> traversed, ktl::string_view name,
                                    fbl::Vector<ktl::string_view>::const_iterator rest,
                                    InserterFn&& inserter) {
    if (name.length() > 256) {
      return fit::error(ZX_ERR_BAD_PATH);
    }

    if (starnix::contains(name, '/')) {
      return fit::error(ZX_ERR_BAD_PATH);
    }

    return ktl::visit(
        TreeBuilder::overloaded{
            [&](Directory& d) -> fit::result<zx_status_t> {
              if (++rest == full_path.end()) {
                return inserter(d.entries_, name, full_path, ktl::move(traversed));
              } else {
                auto next_component = *rest;
                fbl::AllocChecker ac;
                traversed.push_back(name, &ac);
                if (!ac.check()) {
                  return fit::error(ZX_ERR_NO_MEMORY);
                }
                auto entry = d.entries_.find(name);
                if (entry == d.entries_.end()) {
                  auto child = TreeBuilder(ktl::move(Directory()));
                  auto result = child.add_path(full_path, ktl::move(traversed), next_component,
                                               rest, inserter);
                  if (result.is_error()) {
                    return result.take_error();
                  }
                  auto [_it, inserted] = d.entries_.emplace(name, ktl::move(child));
                  ZX_ASSERT(inserted);
                  return fit::ok();

                } else {
                  return entry->second.add_path(full_path, ktl::move(traversed), next_component,
                                                rest, inserter);
                }
              }
            },
            [&](Leaf&) -> fit::result<zx_status_t> { return fit::error(ZX_ERR_BAD_PATH); }},
        variant_);
  }

  // Helper function for building a tree with a default inode generator. Use if you don't
  // care about directory inode values.
  SimpleDirectory* build(FileSystemHandle fs) {
    return build_with_inode_generator(fs, [&fs]() -> ino_t { return fs->next_node_id(); });
  }

  template <typename InodeGenFn>
  SimpleDirectory* build_with_inode_generator(FileSystemHandle fs, InodeGenFn&& fn) {
    return ktl::visit(TreeBuilder::overloaded{
                          [&](Directory& d) -> SimpleDirectory* {
                            fbl::AllocChecker ac;
                            auto res = new (&ac) SimpleDirectory();
                            ASSERT(ac.check());

                            for (auto& [name, child] : d.entries_) {
                              auto result = res->add_entry(name, child.build_dyn(fs, name, fn));
                              ZX_DEBUG_ASSERT_MSG(
                                  result.is_ok(),
                                  "Internal error.  We have already checked all the entry names. \
                             There should be no collisions, nor overly long names.");
                            };
                            return res;
                          },
                          [&](Leaf&) -> SimpleDirectory* {
                            ZX_PANIC("Leaf nodes should not be buildable through the public API.");
                            return nullptr;
                          }},
                      variant_);
  }

 private:
  template <typename InodeGenFn>
  FsNodeHandle build_dyn(FileSystemHandle fs, ktl::string_view dir, InodeGenFn&& gen_id) {
    auto id = gen_id();
    return ktl::visit(
        TreeBuilder::overloaded{
            [&](Directory& d) -> FsNodeHandle {
              fbl::AllocChecker ac;
              auto res = ktl::make_unique<SimpleDirectory>(&ac);
              ASSERT(ac.check());

              for (auto& [name, child] : d.entries_) {
                auto result = res->add_entry(name, child.build_dyn(fs, name, gen_id));
                ZX_DEBUG_ASSERT_MSG(result.is_ok(),
                                    "Internal error.  We have already checked all the entry names. \
                             There should be no collisions, nor overly long names.");
              }
              return fs->create_node_with_id(ktl::move(res), id,
                                             FsNodeInfo::new_factory(mode_, creds_)(id),
                                             Credentials::root());
            },
            [&](Leaf& l) -> FsNodeHandle {
              return fs->create_node_with_id(ktl::move(l.entry_), id,
                                             FsNodeInfo::new_factory(mode_, entry_creds_)(id),
                                             Credentials::root());
            }},
        variant_);
  }

 private:
  TreeBuilder(Directory dir) : variant_(ktl::move(dir)) {}
  TreeBuilder(Leaf leaf) : variant_(ktl::move(leaf)) {}

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<>
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };

  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  ktl::variant<Directory, Leaf> variant_;

  FileMode mode_ = FILE_MODE(IFDIR, 0755);
  FsCred creds_;
  FsCred entry_creds_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_TREE_BUILDER_H_
