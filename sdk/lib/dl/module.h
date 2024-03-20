// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_MODULE_H_
#define LIB_DL_MODULE_H_

#include <lib/elfldltl/soname.h>
#include <lib/fit/result.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>

#include "error.h"

namespace dl {

// TODO(https://fxbug.dev/324136831): comment on how Module relates to startup
// modules when the latter is supported.
// A Module is created for every unique file object loaded either directly or
// indirectly as a dependency of another Module. While this is an internal API,
// a Module* is the void* handle returned by the public <dlfcn.h> API.
class Module : public fbl::DoublyLinkedListable<std::unique_ptr<Module>> {
 public:
  using Soname = elfldltl::Soname<>;

  // Not copyable, but movable.
  Module(const Module&) = delete;
  Module(Module&&) = default;

  constexpr bool operator==(const Soname& other_name) const { return name() == other_name; }

  constexpr const Soname& name() const { return name_; }

  static fit::result<Error, std::unique_ptr<Module>> Create(Soname name, fbl::AllocChecker& ac) {
    auto module = std::unique_ptr<Module>{new (ac) Module};
    if (!ac.check()) {
      return Error::OutOfMemory();
    }
    module->name_ = name;
    return fit::ok(std::move(module));
  }

 private:
  Module() = default;

  // TODO(https://fxbug.dev/324136435): ld::abi::Abi<>::Module::link_map::name
  // will eventually point to this.
  Soname name_;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
