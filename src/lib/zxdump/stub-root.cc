// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zxdump/task.h>

namespace zxdump {
namespace {

constexpr auto NotFuchsia() {
  return fit::error{Error{"Try running Fuchsia!", ZX_ERR_NOT_SUPPORTED}};
}

}  // namespace

fit::result<Error, LiveHandle> GetRootJob() { return NotFuchsia(); }

fit::result<Error, LiveHandle> GetRootResource() { return NotFuchsia(); }

}  // namespace zxdump
