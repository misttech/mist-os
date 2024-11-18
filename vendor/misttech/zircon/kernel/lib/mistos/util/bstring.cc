// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/bstring.h"

#include <ktl/enforce.h>

namespace mtl {

void BString::InitWithEmpty() {
  fbl::AllocChecker ac;
  data_.reserve(1, &ac);
  ZX_ASSERT(ac.check());
  data_.push_back('\0', &ac);
  ZX_ASSERT(ac.check());
}

void BString::Init(const char* data, size_t length) {
  if (length == 0U) {
    InitWithEmpty();
    return;
  }
  fbl::AllocChecker ac;
  data_.reserve(length + 1, &ac);
  ZX_ASSERT(ac.check());
  memcpy(data_.data(), data, length);
  data_.set_size(length);

  data_.push_back('\0', &ac);
  ZX_ASSERT(ac.check());
}

void BString::Init(size_t count, char ch) {
  if (count == 0U) {
    InitWithEmpty();
    return;
  }
  fbl::AllocChecker ac;
  data_.reserve(count + 1, &ac);
  ZX_ASSERT(ac.check());
  memset(data_.data(), ch, count);
  data_.set_size(count);

  data_.push_back('\0', &ac);
  ZX_ASSERT(ac.check());
}

int BString::compare(const BString& other) const {
  size_t len = ktl::min(size(), other.size());
  int retval = memcmp(data(), other.data(), len);
  if (retval == 0) {
    if (size() == other.size()) {
      return 0;
    }
    return size() < other.size() ? -1 : 1;
  }
  return retval;
}

bool operator==(const BString& lhs, const BString& rhs) {
  return lhs.size() == rhs.size() && memcmp(lhs.data(), rhs.data(), lhs.size()) == 0;
}

BString to_string(const BString& str) { return str; }

}  // namespace mtl
