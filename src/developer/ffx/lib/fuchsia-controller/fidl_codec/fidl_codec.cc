// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#define PY_SSIZE_T_CLEAN

#include <Python.h>

#include <fstream>
#include <string>

#include "encode.h"
#include "ir.h"
#include "mod.h"
#include "py_wrapper.h"
#include "src/developer/ffx/lib/fuchsia-controller/abi/macros.h"
#include "src/lib/fidl_codec/library_loader.h"

extern struct PyModuleDef fidl_codec_mod;

namespace {

constexpr PyMethodDef SENTINEL = {nullptr, nullptr, 0, nullptr};

PyMethodDef FidlCodecMethods[] = {
    encode::encode_fidl_message_py_def,
    ir::add_ir_path_py_def,
    SENTINEL,
};

int FidlCodecModule_clear(PyObject *m) {
  auto state = reinterpret_cast<mod::FidlCodecState *>(PyModule_GetState(m));
  state->~FidlCodecState();
  return 0;
}

PyMODINIT_FUNC __attribute__((visibility("default"))) PyInit_fidl_codec() {
  auto m = py::Object(PyModule_Create(&fidl_codec_mod));
  if (m == nullptr) {
    return nullptr;
  }
  auto state = reinterpret_cast<mod::FidlCodecState *>(PyModule_GetState(m.get()));
  new (state) mod::FidlCodecState();
  return m.take();
}

}  // namespace

DES_MIX struct PyModuleDef fidl_codec_mod = {
    PyModuleDef_HEAD_INIT,
    .m_name = "fidl_codec",
    .m_doc = nullptr,
    .m_size = sizeof(mod::FidlCodecState *),
    .m_methods = FidlCodecMethods,
    .m_clear = FidlCodecModule_clear,
};
