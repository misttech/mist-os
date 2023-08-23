// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "handle.h"

#include "convert.h"
#include "mod.h"

namespace handle {

void Handle_dealloc(Handle *self) {
  ffx_close_handle(self->handle);
  Py_TYPE(self)->tp_free(reinterpret_cast<PyObject *>(self));
}

int Handle_init(Handle *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"handle", nullptr};
  zx_handle_t handle;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "I", const_cast<char **>(kwlist), &handle)) {
    return -1;
  }
  if (handle == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
    return -1;
  }
  self->handle = handle;
  return 0;
}

PyObject *Handle_as_int(Handle *self, PyObject *Py_UNUSED(arg)) {
  return PyLong_FromUnsignedLongLong(self->handle);
}

PyObject *Handle_take(Handle *self, PyObject *Py_UNUSED(arg)) {
  auto result = PyLong_FromUnsignedLongLong(self->handle);
  self->handle = 0;
  return result;
}

PyObject *Handle_close(Handle *self, PyObject *Py_UNUSED(arg)) {
  ffx_close_handle(self->handle);
  self->handle = 0;
  Py_RETURN_NONE;
}

PyMethodDef Channel_methods[] = {
    {"as_int", reinterpret_cast<PyCFunction>(Handle_as_int), METH_NOARGS, nullptr},
    {"take", reinterpret_cast<PyCFunction>(Handle_take), METH_NOARGS,
     "Takes the underlying fidl handle, setting it internally to zero (thus invalidating the "
     "underlying channel). This is used for sending a handle through FIDL function calls."},
    {"close", reinterpret_cast<PyCFunction>(Handle_close), METH_NOARGS,
     "Closes the underlying handle. This will invalidate any other copies of this channel."},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject HandleType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.Handle",
    .tp_basicsize = sizeof(Handle),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(Handle_dealloc),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller FIDL handle. This is used to bootstrap processes for FIDL interactions.",
    .tp_init = reinterpret_cast<initproc>(Handle_init),
    .tp_new = PyType_GenericNew,
};

}  // namespace handle
