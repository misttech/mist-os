// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DRIVER_SYMBOLS_SYMBOLS_H_
#define SRC_LIB_DRIVER_SYMBOLS_SYMBOLS_H_

#include <lib/zx/result.h>
#include <lib/zx/vmo.h>

#include <set>
#include <vector>

namespace driver_symbols {

// Checks the |driver_vmo| for usage of restricted symbols.
// Returns a vector of any restricted symbols, or an error if the vmo is not an ELF file.
zx::result<std::vector<std::string>> FindRestrictedSymbols(zx::vmo& driver_vmo);

}  // namespace driver_symbols

#endif  // SRC_LIB_DRIVER_SYMBOLS_SYMBOLS_H_
