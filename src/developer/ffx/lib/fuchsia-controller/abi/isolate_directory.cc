// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "isolate_directory.h"

#include "mod.h"

namespace isolate {

void IsolateDir_dealloc(IsolateDir *self) {
  // If the directory is never successfully created on sending it to a context, then
  // nothing happens.
  if (std::filesystem::is_directory(self->dir)) {
    std::filesystem::remove_all(self->dir);
  }
}

int IsolateDir_init(IsolateDir *self, PyObject *args, PyObject *kwds) {  // NOLINT
  static const char *kwlist[] = {"dir", nullptr};
  const char *dir_cstr = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "s", const_cast<char **>(kwlist), &dir_cstr)) {
    return -1;
  }
  self->dir = std::filesystem::path(dir_cstr);
  return 0;
}

PyObject *IsolateDir_directory(IsolateDir *self, PyObject *Py_UNUSED(arg)) {
  return PyUnicode_FromString(self->dir.c_str());
}

PyMethodDef IsolateDir_methods[] = {
    {"directory", reinterpret_cast<PyCFunction>(IsolateDir_directory), METH_NOARGS,
     "Returns a string representing the directory to which this IsolateDir points. It may or may not have been created."},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject IsolateDirType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.IsolateDir",
    .tp_basicsize = sizeof(IsolateDir),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(IsolateDir_dealloc),
    .tp_doc =
        "Fuchsia controller Isolate Directory. Represents an Isolate Directory path to be used by the fuchsia controller Context object. This object cleans up the Isolate Directory (if it exists) once it goes out of scope.",
    .tp_init = reinterpret_cast<initproc>(IsolateDir_init),
    .tp_new = PyType_GenericNew,
};

}  // namespace isolate
