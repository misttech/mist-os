// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_CHANNEL_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_CHANNEL_H_

#include <zircon/types.h>

#include "macros.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"

namespace channel {

extern PyTypeObject *ChannelType;

int ChannelTypeInit();

IGNORE_EXTRA_SC
using Channel = struct {
  PyObject_HEAD;
  zx_handle_t handle;
};

}  // namespace channel

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_CHANNEL_H_
