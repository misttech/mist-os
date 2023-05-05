// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_HANDLE_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_HANDLE_H_

#include <Python.h>
#include <zircon/types.h>

#include "src/developer/ffx/lib/fuchsia-controller/abi/macros.h"

namespace fidl_handle {

extern PyTypeObject FidlHandleType;

IGNORE_EXTRA_SC
using FidlHandle = struct {
  PyObject_HEAD;
  zx_handle_t handle;
};

}  // namespace fidl_handle

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_HANDLE_H_
