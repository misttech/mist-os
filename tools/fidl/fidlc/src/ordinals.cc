// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/ordinals.h"

#include <zircon/assert.h>

#define BORINGSSL_NO_CXX
#include <openssl/sha.h>

#include "tools/fidl/fidlc/src/attributes.h"
#include "tools/fidl/fidlc/src/raw_ast.h"

namespace fidlc {

std::string GetSelector(const AttributeList* attributes, SourceSpan name) {
  const Attribute* maybe_selector_attr = attributes->Get("selector");
  if (maybe_selector_attr != nullptr) {
    auto selector_constant = maybe_selector_attr->GetArg(AttributeArg::kDefaultAnonymousName);
    if (selector_constant != nullptr && selector_constant->value->IsResolved()) {
      ZX_ASSERT(selector_constant->value->Value().kind == ConstantValue::Kind::kString);
      auto& selector_string_constant =
          static_cast<const StringConstantValue&>(selector_constant->value->Value());
      return selector_string_constant.MakeContents();
    }
  }
  return std::string(name.data().data(), name.data().size());
}

namespace {

uint64_t CalcOrdinal(const std::string_view& full_name) {
  uint8_t digest[SHA256_DIGEST_LENGTH];
  SHA256(reinterpret_cast<const uint8_t*>(full_name.data()), full_name.size(), digest);
  // The following dance ensures that we treat the bytes as a little-endian
  // int64 regardless of host byte order.
  // clang-format off
  uint64_t ordinal =
      static_cast<uint64_t>(digest[0]) |
      static_cast<uint64_t>(digest[1]) << 8 |
      static_cast<uint64_t>(digest[2]) << 16 |
      static_cast<uint64_t>(digest[3]) << 24 |
      static_cast<uint64_t>(digest[4]) << 32 |
      static_cast<uint64_t>(digest[5]) << 40 |
      static_cast<uint64_t>(digest[6]) << 48 |
      static_cast<uint64_t>(digest[7]) << 56;
  // clang-format on

  return ordinal & 0x7fffffffffffffff;
}

}  // namespace

RawOrdinal64 GetGeneratedOrdinal64(const std::vector<std::string_view>& library_name,
                                   const std::string_view& protocol_name,
                                   const std::string_view& selector_name,
                                   const SourceElement& source_element) {
  if (selector_name.find('/') != std::string_view::npos)
    return RawOrdinal64(source_element, CalcOrdinal(selector_name));

  // TODO(https://fxbug.dev/118282): Move this closer (code wise) to NameFlatName, ideally sharing
  // code.
  std::string full_name;
  bool once = false;
  for (std::string_view id : library_name) {
    if (once) {
      full_name.push_back('.');
    } else {
      once = true;
    }
    full_name.append(id.data(), id.size());
  }
  full_name.append("/");
  full_name.append(protocol_name.data(), protocol_name.size());
  full_name.append(".");
  full_name.append(selector_name);

  return RawOrdinal64(source_element, CalcOrdinal(full_name));
}

}  // namespace fidlc
