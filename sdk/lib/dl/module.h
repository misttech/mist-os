// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_MODULE_H_
#define LIB_DL_MODULE_H_

#include <lib/elfldltl/soname.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>

namespace dl {

// TODO(https://fxbug.dev/324136831): comment on how Module relates to startup
// modules when the latter is supported.
// A Module is created for every unique file object loaded either directly or
// indirectly as a dependency of another Module. While this is an internal API,
// a Module* is the void* handle returned by the public <dlfcn.h> API.
class Module : public fbl::DoublyLinkedListable<std::unique_ptr<Module>> {
 public:
  using Soname = elfldltl::Soname<>;

  // Not copyable, not movable.
  Module(const Module&) = delete;
  Module(Module&&) = delete;

  constexpr bool operator==(const Soname& other_name) const { return name() == other_name; }

  constexpr const Soname& name() const { return name_; }

  static std::unique_ptr<Module> Create(Soname name, fbl::AllocChecker& ac) {
    // TODO(https://fxbug.dev/328487096): use the AllocChecker and handle
    // allocation failures.
    auto module = std::unique_ptr<Module>{new Module};
    module->name_ = name;
    return module;
  }

 private:
  Module() = default;

  // TODO(https://fxbug.dev/324136435): ld::abi::Abi<>::Module::link_map::name
  // will eventually point to this.
  Soname name_;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
