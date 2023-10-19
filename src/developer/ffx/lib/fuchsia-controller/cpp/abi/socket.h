// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_SOCKET_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_SOCKET_H_

#include <zircon/types.h>

#include "macros.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"

namespace socket {

extern PyTypeObject *SocketType;

int SocketTypeInit();

IGNORE_EXTRA_SC
using Socket = struct {
  PyObject_HEAD;
  zx_handle_t handle;
};

}  // namespace socket

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_SOCKET_H_
