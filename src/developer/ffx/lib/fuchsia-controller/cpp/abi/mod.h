// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_MOD_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_MOD_H_

#include "fuchsia_controller.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"

namespace mod {

constexpr uint64_t ERR_SCRATCH_LEN = 1024;

// Definition of the module-wide state.
using FuchsiaControllerState = struct {
  char ERR_SCRATCH[ERR_SCRATCH_LEN];
  ffx_lib_context_t *ctx;
};

FuchsiaControllerState *get_module_state();
void dump_python_err();

inline int GenericTypeInit(PyTypeObject **type, PyType_Spec *spec) {
  assert(type != nullptr);
  *type = PyTypeCast(PyType_FromSpec(spec));
  if (*type == nullptr) {
    return -1;
  }
  if (PyType_Ready(*type) < 0) {
    return -1;
  }
  return 1;
}

}  // namespace mod

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_MOD_H_
