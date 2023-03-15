// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_SYMBOLS_SYMBOLS_H_
#define LIB_DRIVER_SYMBOLS_SYMBOLS_H_

#include <lib/fdf/dispatcher.h>
#include <zircon/fidl.h>

struct EncodedDriverStartArgs {
  // |msg| is an encoded `fuchsia.driver.framework/DriverStartArgs` table. The
  // ownership of handles in |msg| are transferred to the driver. The driver may
  // mutate the bytes referenced by |msg|, but those are only alive until the
  // |DriverLifecycleV1::start| method returns.
  fidl_incoming_msg_t* msg;

  // |wire_format_metadata| describes the the revision of the FIDL wire format
  // used to encode |msg|.
  fidl_opaque_wire_format_metadata wire_format_metadata;
};

// The callback to trigger when done preparing for stop to be invoked.
// Once this is called, the framework will shutdown all driver dispatchers
// belonging to the driver, and then call the stop hook in the DriverLifecycle.
//
// |cookie| is the opaque pointer that the |prepare_stop| caller passed as the |complete_cookie|.
// |status| is the status of the |prepare_stop| async operation.
typedef void(PrepareStopCompleteCallback)(void* cookie, zx_status_t status);

struct DriverLifecycle {
  // This is the version of `DriverLifecycle` and all structures used by it.
  uint64_t version;

  struct v1 {
    // Pointer to a function that can start execution of the driver. This
    // function is executed on the shared driver thread within a `driver_host`.
    //
    // |start_args| contains the arguments for starting a driver.
    // |dispatcher| is the default fdf dispatcher on which to run the driver.
    // The driver is free to ignore this and use its own.
    // |driver| provides a place to store the opaque driver structure.
    zx_status_t (*start)(EncodedDriverStartArgs start_args, fdf_dispatcher_t* dispatcher,
                         void** driver);

    // Pointer to a function that can stop execution of the driver. This function
    // is executed on the shared driver thread within a `driver_host`.
    //
    // |driver| is the value that was stored when the driver was started.
    zx_status_t (*stop)(void* driver);
  } v1;
  struct v2 {
    // Pointer to a function that is triggered before the stop hook is invoked. This method allows
    // the driver to asynchronously do work that may not otherwise be done synchronously during the
    // stop hook.
    // |driver| is the value that was stored when the driver was started.
    // |complete| is the callback that must be called to complete the |prepare_stop| operation.
    // |complete_cookie| is an opaque pointer provided by the caller (the Driver Framework), and
    // must be provided without modification into the |complete| callback function.
    void (*prepare_stop)(void* driver, PrepareStopCompleteCallback* complete,
                         void* complete_cookie);
  } v2;
};

#define FUCHSIA_DRIVER_LIFECYCLE_V1(start, stop)                           \
  extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__ __EXPORT { \
    .version = 1, .v1 = {start, stop}, .v2 = {nullptr},                    \
  }

#define FUCHSIA_DRIVER_LIFECYCLE_V2(start, prepare_stop, stop)             \
  extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__ __EXPORT { \
    .version = 2, .v1 = {start, stop}, .v2 = {prepare_stop},               \
  }

#endif  // LIB_DRIVER_SYMBOLS_SYMBOLS_H_
