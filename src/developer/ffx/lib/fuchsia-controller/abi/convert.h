// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_CONVERT_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_CONVERT_H_
#include <Python.h>
#include <zircon/types.h>

#include <sstream>
#include <string>

namespace convert {

static constexpr uint32_t MINUS_ONE_U32 = std::numeric_limits<uint32_t>::max();
static constexpr uint64_t MINUS_ONE_U64 = std::numeric_limits<uint64_t>::max();
static_assert(sizeof(unsigned long long) == sizeof(uint64_t));  // NOLINT

inline uint32_t PyLong_AsU32(PyObject *py_long) {
  auto res = PyLong_AsUnsignedLongLong(py_long);
  if (res > static_cast<uint64_t>(MINUS_ONE_U32)) {
    std::stringstream ss;
    ss << "Value " << res << " too large for u32";
    PyErr_SetString(PyExc_OverflowError, ss.str().c_str());
    return MINUS_ONE_U32;
  }
  return static_cast<uint32_t>(res);
}

inline uint64_t PyLong_AsU64(PyObject *py_long) {
  return static_cast<uint64_t>(PyLong_AsUnsignedLongLong(py_long));
}

}  // namespace convert
#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_CONVERT_H_
