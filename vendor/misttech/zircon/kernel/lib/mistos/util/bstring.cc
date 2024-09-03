// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/bstring.h"

#include <ktl/enforce.h>

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
