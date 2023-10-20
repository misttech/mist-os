// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/common/version.h"

namespace zxdb {

// TODO(b/306473682): Remove this when the dependency in //src/lib/analytics/cpp/core_dev_tools is
// removed.
const char* kBuildVersion = "1.0";

}  // namespace zxdb
