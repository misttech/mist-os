// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_HANDLE_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_HANDLE_H_

#include <Python.h>

#include "fuchsia_controller.h"
#include "macros.h"

namespace fidl_handle {

extern PyTypeObject FidlHandleType;

IGNORE_EXTRA_SC
using FidlHandle = struct {
  PyObject_HEAD;
  zx_handle_t handle;
};

inline zx_handle_t RawHandleFromU64Checked(uint64_t h) {
  constexpr uint64_t U32_MAX = 0xffffffff;
  if (h > U32_MAX) {
    PyErr_SetString(PyExc_OverflowError, "handle value larger than uint32_t");
    return -1;
  }
  return static_cast<zx_handle_t>(h);
}

// Converts a PyLong object into a valid zx_handle_t with checks.
//
// If something failed during conversion, check if the value returned
// is equal to `static_cast<zx_handle_t>(-1) && PyErr_Occurred()`. This
// means a failure has occurred and the appropriate Python error has been set,
// similar to other Python conversion functions.
//
// Expects that the PyObject being passed is already a PyLong.
inline zx_handle_t RawHandleFromPyLong(PyObject *py_long) {
  uint64_t res = PyLong_AsUnsignedLong(py_long);
  if (res == static_cast<uint64_t>(-1) && PyErr_Occurred()) {
    return -1;
  }
  return RawHandleFromU64Checked(res);
}

}  // namespace fidl_handle

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_HANDLE_H_
