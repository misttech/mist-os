// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#define PY_SSIZE_T_CLEAN

#include <Python.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iostream>
#include <memory>
#include <sstream>
#include <string>

#include "error.h"
#include "fidl_channel.h"
#include "fuchsia_controller.h"
#include "macros.h"
#include "mod.h"

extern struct PyModuleDef fuchsia_controller_py;

namespace {

constexpr PyMethodDef SENTINEL = {nullptr, nullptr, 0, nullptr};

IGNORE_EXTRA_SC
using Context = struct {
  PyObject_HEAD;
  ffx_env_context_t *env_context;
};

void Context_dealloc(Context *self) {
  destroy_ffx_env_context(self->env_context);
  Py_TYPE(self)->tp_free(reinterpret_cast<PyObject *>(self));
}

int Context_init(Context *self, PyObject *args, PyObject *kwds) {
  static const char TYPE_ERROR[] = "`config` must be a dictionary of string key/value pairs";
  static const char *kwlist[] = {"config", nullptr};
  PyObject *config = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "O", const_cast<char **>(kwlist), &config)) {
    return -1;
  }
  if (!config) {
    return -1;
  }
  if (!PyDict_Check(config)) {
    PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
    return -1;
  }
  Py_ssize_t config_len = PyDict_Size(config);
  if (config_len < 0) {
    return -1;
  }
  std::unique_ptr<ffx_config_t[]> ffx_config;
  if (config_len > 0) {
    ffx_config = std::make_unique<ffx_config_t[]>(config_len);
  }
  PyObject *py_key = nullptr;
  PyObject *py_value = nullptr;
  Py_ssize_t pos = 0;
  // `pos` is not used for iterating in ffx_config because it is an internal
  // iterator for a sparse map, so does not always increment by one.
  for (Py_ssize_t i = 0; PyDict_Next(config, &pos, &py_key, &py_value); ++i) {
    if (!PyUnicode_Check(py_key)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return -1;
    }
    const char *key = PyUnicode_AsUTF8(py_key);
    if (key == nullptr) {
      return -1;
    }
    if (!PyUnicode_Check(py_value)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return -1;
    }
    const char *value = PyUnicode_AsUTF8(py_value);
    if (value == nullptr) {
      return -1;
    }
    ffx_config[i] = {
        .key = key,
        .value = value,
    };
  }
  if (create_ffx_env_context(&self->env_context, mod::get_module_state()->ctx, ffx_config.get(),
                             config_len) != ZX_OK) {
    mod::dump_python_err();
    return -1;
  }
  return 0;
}

PyObject *Context_open_daemon_protocol(Context *self, PyObject *protocol) {
  const char *c_protocol = PyUnicode_AsUTF8(protocol);
  if (c_protocol == nullptr) {
    return nullptr;
  }
  zx_handle_t handle;
  if (ffx_open_daemon_protocol(self->env_context, c_protocol, &handle) != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&fidl_channel::FidlChannelType), "I",
                               handle);
}

PyObject *Context_open_target_proxy(Context *self, PyObject *Py_UNUSED(unused)) {
  zx_handle_t handle;
  if (ffx_open_target_proxy(self->env_context, &handle) != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&fidl_channel::FidlChannelType), "I",
                               handle);
}

PyObject *Context_open_device_proxy(Context *self, PyObject *args) {
  char *c_moniker;
  char *c_capability_name;
  if (!PyArg_ParseTuple(args, "ss", &c_moniker, &c_capability_name)) {
    return nullptr;
  }
  zx_handle_t handle;
  if (ffx_open_device_proxy(self->env_context, c_moniker, c_capability_name, &handle) != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&fidl_channel::FidlChannelType), "I",
                               handle);
}

PyMethodDef Context_methods[] = {
    {"open_daemon_protocol", reinterpret_cast<PyCFunction>(Context_open_daemon_protocol), METH_O,
     nullptr},
    {"open_target_proxy", reinterpret_cast<PyCFunction>(Context_open_target_proxy), METH_NOARGS,
     nullptr},
    {"open_device_proxy", reinterpret_cast<PyCFunction>(Context_open_device_proxy), METH_VARARGS,
     nullptr},
    SENTINEL};

DES_MIX PyTypeObject ContextType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.Context",
    .tp_basicsize = sizeof(Context),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(Context_dealloc),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller context. This is the necessary object for interacting with a Fuchsia device.",
    .tp_methods = Context_methods,
    .tp_init = reinterpret_cast<initproc>(Context_init),
    .tp_new = PyType_GenericNew,
};

PyObject *open_handle_notifier(PyObject *self, PyObject *Py_UNUSED(arg)) {
  auto descriptor = ffx_open_handle_notifier(mod::get_module_state()->ctx);
  if (descriptor <= 0) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyLong_FromLong(descriptor);
}

PyMethodDef FuchsiaControllerMethods[] = {
    {"open_handle_notifier", open_handle_notifier, METH_NOARGS,
     "Open the handle notification descriptor. Can only be done once within a module."},
    SENTINEL,
};

int FuchsiaControllerModule_clear(PyObject *m) {
  auto state = reinterpret_cast<mod::FuchsiaControllerState *>(PyModule_GetState(m));
  destroy_ffx_lib_context(state->ctx);
  return 0;
}

PyMODINIT_FUNC __attribute__((visibility("default"))) PyInit_fuchsia_controller_py() {
  if (PyType_Ready(&ContextType) < 0) {
    return nullptr;
  }
  if (PyType_Ready(&fidl_handle::FidlHandleType) < 0) {
    return nullptr;
  }
  if (PyType_Ready(&fidl_channel::FidlChannelType) < 0) {
    return nullptr;
  }
  PyObject *m = PyModule_Create(&fuchsia_controller_py);
  if (m == nullptr) {
    return nullptr;
  }
  auto state = reinterpret_cast<mod::FuchsiaControllerState *>(PyModule_GetState(m));
  create_ffx_lib_context(&state->ctx, state->ERR_SCRATCH, mod::ERR_SCRATCH_LEN);
  Py_INCREF(&ContextType);
  if (PyModule_AddObject(m, "Context", reinterpret_cast<PyObject *>(&ContextType)) < 0) {
    Py_DECREF(&ContextType);
    Py_DECREF(m);
    return nullptr;
  }
  auto zx_status_type = error::ZxStatusType_Create();
  if (PyModule_AddObject(m, "ZxStatus", reinterpret_cast<PyObject *>(zx_status_type)) < 0) {
    Py_DECREF(zx_status_type);
    Py_DECREF(m);
    return nullptr;
  }
  Py_INCREF(&fidl_handle::FidlHandleType);
  if (PyModule_AddObject(m, "FidlHandle",
                         reinterpret_cast<PyObject *>(&fidl_handle::FidlHandleType)) < 0) {
    Py_DECREF(&fidl_handle::FidlHandleType);
    Py_DECREF(m);
    return nullptr;
  }
  Py_INCREF(&fidl_channel::FidlChannelType);
  if (PyModule_AddObject(m, "FidlChannel",
                         reinterpret_cast<PyObject *>(&fidl_channel::FidlChannelType)) < 0) {
    Py_DECREF(&fidl_channel::FidlChannelType);
    Py_DECREF(m);
    return nullptr;
  }
  return m;
}

}  // namespace

DES_MIX struct PyModuleDef fuchsia_controller_py = {
    PyModuleDef_HEAD_INIT,
    .m_name = "fuchsia_controller_py",
    .m_doc = nullptr,
    .m_size = sizeof(mod::FuchsiaControllerState),
    .m_methods = FuchsiaControllerMethods,
    .m_clear = FuchsiaControllerModule_clear,
};
