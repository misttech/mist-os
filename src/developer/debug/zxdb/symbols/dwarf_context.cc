// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/symbols/dwarf_context.h"

#include "llvm/DebugInfo/DWARF/DWARFContext.h"
#include "llvm/Support/Error.h"
#include "src/developer/debug/shared/logging/logging.h"

namespace {

// This function exists to filter out specific errors when decompressing
// .debug_gdb_scripts sections from compressed debuginfo files which fail in
// LLVM.
void LLVMErrorHandler(llvm::Error error) {
  llvm::handleAllErrors(std::move(error), [](llvm::ErrorInfoBase& info) {
    if (info.message().find("gdb_scripts")) {
      // Suppress errors for decompressing the "debug_gdb_scripts" section in rust binaries.
      return;
    }

    // Otherwise just pass through to the console like the defaut error handler.
    LOGS(Error) << info.message();
  });
}

}  // namespace

namespace zxdb {

std::unique_ptr<llvm::DWARFContext> GetDwarfContext(llvm::object::ObjectFile* object_file) {
  // Overwrite the default Error handler object, but leave everything else default.
  return llvm::DWARFContext::create(*object_file,
                                    llvm::DWARFContext::ProcessDebugRelocations::Process, nullptr,
                                    "", &LLVMErrorHandler);
}

}  // namespace zxdb
