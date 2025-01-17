// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This is just a wrapper header for including Python that enforces the limited API has been set,
// to ensure ABI compatibility.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FUCHSIA_CONTROLLER_ABI_ABI_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FUCHSIA_CONTROLLER_ABI_ABI_H_

#define PY_SSIZE_T_CLEAN
#define Py_LIMITED_API 0x030b00f0
#include <Python.h>

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FUCHSIA_CONTROLLER_ABI_ABI_H_
