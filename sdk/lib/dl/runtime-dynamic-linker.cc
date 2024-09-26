// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-dynamic-linker.h"

namespace dl {

RuntimeModule* RuntimeDynamicLinker::FindModule(Soname name) {
  if (auto it = std::find(modules_.begin(), modules_.end(), name); it != modules_.end()) {
    // TODO(https://fxbug.dev/328135195): increase reference count.
    // TODO(https://fxbug.dev/326120230): update flags
    RuntimeModule& found = *it;
    return &found;
  }
  return nullptr;
}

fit::result<Error, void*> RuntimeDynamicLinker::LookupSymbol(const RuntimeModule& root,
                                                             const char* ref) {
  Diagnostics diag;
  elfldltl::SymbolName name{ref};
  // TODO(https://fxbug.dev/338229633): use elfldltl::MakeSymbolResolver.
  for (const RuntimeModule* module : root.module_tree()) {
    if (const auto* sym = name.Lookup(module->symbol_info())) {
      if (sym->type() == elfldltl::ElfSymType::kTls) {
        diag.SystemError(
            "TODO(https://fxbug.dev/331421403): TLS semantics for dlsym() are not supported yet.");
        return diag.take_error();
      }
      return diag.ok(reinterpret_cast<void*>(sym->value + module->load_bias()));
    }
  }
  diag.UndefinedSymbol(ref);
  return diag.take_error();
}

}  // namespace dl
