// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_DWARF_CONTEXT_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_DWARF_CONTEXT_H_

#include <memory>

namespace llvm {
class DWARFContext;
namespace object {
class ObjectFile;
}  // namespace object
}  // namespace llvm

namespace zxdb {

// Use this function to construct a new llvm::DWARFContext from the given
// ObjectFile. Most everything in the resulting object is default, but we use a
// custom error handling function to suppress certain errors in LLVM that can
// come from decompressing ELF sections in Rust programs.
std::unique_ptr<llvm::DWARFContext> GetDwarfContext(llvm::object::ObjectFile* object_file);

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_DWARF_CONTEXT_H_
