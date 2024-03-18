// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
#define LIB_DL_RUNTIME_DYNAMIC_LINKER_H_

#include <dlfcn.h>  // for RTLD_* macros
#include <lib/elfldltl/soname.h>
#include <lib/fit/result.h>

#include <fbl/intrusive_double_list.h>

#include "error.h"
#include "module.h"

namespace dl {

enum OpenSymbolScope : int {
  kLocal = RTLD_LOCAL,
  kGlobal = RTLD_GLOBAL,
};

enum OpenBindingMode : int {
  kNow = RTLD_NOW,
  // RTLD_LAZY functionality is not supported, but keep the flag definition
  // because it's a legitimate flag that can be passed in.
  kLazy = RTLD_LAZY,
};

enum OpenFlags : int {
  kNoload = RTLD_NOLOAD,
  kNodelete = RTLD_NODELETE,
  // TODO(https://fxbug.dev/323425900): support glibc's RTLD_DEEPBIND flag.
  // kDEEPBIND = RTLD_DEEPBIND,
};

// Masks used to validate flag values.
inline constexpr int kOpenSymbolScopeMask = OpenSymbolScope::kLocal | OpenSymbolScope::kGlobal;
inline constexpr int kOpenBindingModeMask = OpenBindingMode::kLazy | OpenBindingMode::kNow;
inline constexpr int kOpenFlagsMask = OpenFlags::kNoload | OpenFlags::kNodelete;

class RuntimeDynamicLinker {
 public:
  using Soname = elfldltl::Soname<>;

  // Not copyable, not movable
  RuntimeDynamicLinker() = default;
  RuntimeDynamicLinker(const RuntimeDynamicLinker&) = delete;
  RuntimeDynamicLinker(RuntimeDynamicLinker&&) = delete;

  fit::result<Error, void*> Open(const char* file, int mode);

 private:
  Module* FindModule(const Soname& name);

  // The RuntimeDynamicLinker owns the list of all 'live' modules that have been
  // loaded into the system image.
  // TODO(https://fxbug.dev/324136831): support startup modules
  fbl::DoublyLinkedList<std::unique_ptr<Module>> loaded_modules_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
