// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FLAT_SOURCED_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FLAT_SOURCED_H_

#include <optional>

#include "tools/fidl/fidlc/include/fidl/raw_ast.h"

namespace fidl::flat {

class Sourced {
 public:
  explicit Sourced(raw::SourceElement::Signature signature) : signature_(signature) {}

  const raw::SourceElement::Signature& source_signature() const { return signature_; }

 private:
  raw::SourceElement::Signature signature_;
};

// Similar to |Sourced|, but slightly modified for cases where some instances of the derived class
// may be compiler generated, like a named |TypeConstructor|.
class MaybeSourced {
 public:
  explicit MaybeSourced(std::optional<raw::SourceElement::Signature> maybe_signature)
      : maybe_signature_(maybe_signature) {}

  const std::optional<raw::SourceElement::Signature>& maybe_source_signature() const {
    return maybe_signature_;
  }

 private:
  std::optional<raw::SourceElement::Signature> maybe_signature_;
};

}  // namespace fidl::flat

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FLAT_SOURCED_H_
