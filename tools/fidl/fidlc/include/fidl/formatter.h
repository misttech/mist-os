// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FORMATTER_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FORMATTER_H_

#include <utility>

#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/raw_ast.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/token_list.h"

namespace fidl::fmt {

class NewFormatter final {
 public:
  explicit NewFormatter(size_t cols, Reporter* reporter) : cols_(cols), reporter_(reporter) {}

  std::optional<std::string> Format(const fidl::SourceFile& source_file,
                                    const fidl::ExperimentalFlags& experimental_flags) const;

  std::optional<std::string> Format(std::unique_ptr<raw::File> ast,
                                    fidl::raw::TokenPointerList token_pointer_list,
                                    size_t original_file_size) const;

 private:
  std::string Print(std::unique_ptr<raw::File> ast, fidl::raw::TokenPointerList token_pointer_list,
                    size_t original_file_size) const;

  const size_t cols_;
  Reporter* reporter_;
};

}  // namespace fidl::fmt

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FORMATTER_H_
