// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This is just a wrapper header for including Python that enforces the limited API has been set,
// to ensure ABI compatibility.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_PYTHON_PY_HEADER_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_PYTHON_PY_HEADER_H_

#define PY_SSIZE_T_CLEAN
#define Py_LIMITED_API 0x030b00f0
#include <Python.h>

// Just convenience functions that ensures type-checking of the input being cast.
inline PyObject* PyObjCast(PyTypeObject* obj) { return reinterpret_cast<PyObject*>(obj); }
inline PyTypeObject* PyTypeCast(PyObject* obj) { return reinterpret_cast<PyTypeObject*>(obj); }

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_PYTHON_PY_HEADER_H_
