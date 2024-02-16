// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include "../runtime-dynamic-linker.h"

namespace dl::testing {

fit::result<Error, void*> DlImplTests::DlOpen(const char* name, int mode) {
  return dynamic_linker_.Open(name, mode);
}

}  // namespace dl::testing
