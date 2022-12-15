// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/formatter.h"

#include "tools/fidl/fidlc/include/fidl/lexer.h"
#include "tools/fidl/fidlc/include/fidl/parser.h"
#include "tools/fidl/fidlc/include/fidl/span_sequence.h"
#include "tools/fidl/fidlc/include/fidl/span_sequence_tree_visitor.h"
#include "tools/fidl/fidlc/include/fidl/token_list.h"

namespace fidl::fmt {

std::optional<std::string> NewFormatter::Format(
    const fidl::SourceFile& source_file, const fidl::ExperimentalFlags& experimental_flags) const {
  fidl::Lexer lexer(source_file, reporter_);
  fidl::Parser parser(&lexer, reporter_, experimental_flags);
  std::unique_ptr<raw::File> ast = parser.Parse();
  if (parser.Success()) {
    raw::TokenPointerList token_pointer_list = raw::TokenPointerListBuilder(ast).Build();
    return Print(std::move(ast), std::move(token_pointer_list), source_file.data().size());
  }
  return std::nullopt;
}

std::optional<std::string> NewFormatter::Format(std::unique_ptr<raw::File> ast,
                                                fidl::raw::TokenPointerList token_pointer_list,
                                                size_t original_file_size) const {
  return Print(std::move(ast), std::move(token_pointer_list), original_file_size);
}

std::string NewFormatter::Print(std::unique_ptr<raw::File> ast,
                                fidl::raw::TokenPointerList token_pointer_list,
                                size_t original_file_size) const {
  std::string out;
  auto visitor =
      SpanSequenceTreeVisitor(ast->span().source_file().data(), std::move(token_pointer_list));
  visitor.OnFile(ast);
  MultilineSpanSequence result = visitor.Result();

  // A good guess for the size of the formatted file is just the size of the original, possibly
  // unformatted file.  Reserve that space now to minimize allocations as the printed string grows
  // down the road.
  out.reserve(original_file_size);
  result.Print(cols_, std::nullopt, 0, false, AdjacentIndents(false, false), &out);

  // Formatted files always have a trailing newline.
  if (!out.empty() && out.at(out.size() - 1) != '\n')
    return out + '\n';
  return out;
}
}  // namespace fidl::fmt
