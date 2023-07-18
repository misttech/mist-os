// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_ZIRCON_H_
#define LIB_LD_ZIRCON_H_

#include <lib/zx/debuglog.h>
#include <lib/zx/socket.h>

namespace ld {

// This collects the data from the bootstrap channel.
struct StartupData {
  zx::debuglog debuglog;
  zx::socket log_socket;
};

}  // namespace ld

#endif  // LIB_LD_ZIRCON_H_
