// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_SYMBOLS_SYMBOLS_H_
#define LIB_DRIVER_SYMBOLS_SYMBOLS_H_

#include <lib/fdf/dispatcher.h>
#include <zircon/fidl.h>

// Lightweight reference to an encoded FIDL message.
//
// You may use |fdf_internal::AdoptEncodedFidlMessage| to take ownership of the handles
// in the message.
//
// It is intended as a stable ABI.
struct EncodedFidlMessage {
  uint8_t* bytes;
  zx_handle_t* handles;
  uint32_t num_bytes;
  uint32_t num_handles;
};

struct EncodedDriverStartArgs {
  // |msg| is an encoded `fuchsia.driver.framework/DriverStartArgs` table. The
  // ownership of handles in |msg| are transferred to the driver. The driver may
  // mutate the bytes referenced by |msg|, but those are only alive until the
  // |DriverLifecycle::v1::start| method returns.
  EncodedFidlMessage msg;

  // |wire_format_metadata| describes the the revision of the FIDL wire format
  // used to encode |msg|. This is required in order to be able to correctly decode the
  // |msg| into a FIDL binding.
  fidl_opaque_wire_format_metadata wire_format_metadata;
};

// The callback to trigger when done preparing for stop to be invoked.
// Once this is called, the framework will shutdown all driver dispatchers
// belonging to the driver, and then call the stop hook in the DriverLifecycle.
//
// |cookie| is the opaque pointer that the |prepare_stop| caller passed as the |complete_cookie|.
// |status| is the status of the |prepare_stop| async operation.
typedef void(PrepareStopCompleteCallback)(void* cookie, zx_status_t status);

// Drivers should finish their initial setup and enumeration before calling the start callback. In
// particular they should enumerate all currently available nodes by utilizing
// `fuchsia.driver.framework/Node.AddChild` and waiting for all calls to be completed.
//
// The Framework will not consider the driver to be started until this callback is called. A driver
// will not have PrepareStop or Stop called on it until after it has replied to Start successfully.
// If a driver returns an error to this callback it will not have PrepareStop or Stop called on it,
// so it should have performed all necessary cleanup before returning an error.
//
// |cookie| is the opaque pointer that the |start| caller passed as the |complete_cookie|.
// |status| is the status of the |start| async operation.
// |driver| is the opaque pointer handed back to the driver during future |prepare_stop| and |stop|
// callback invocations.
typedef void(StartCompleteCallback)(void* cookie, zx_status_t status, void* driver);

// The |DriverLifecycle| is the ABI for drivers to expose themselves to the Driver Framework.
// The driver is loaded in as a shared library (also referred to as a DSO), and the global symbol
// `__fuchsia_driver_lifecycle__` is used by the Driver Framework to locate this DriverLifecycle in
// the driver library. The framework will use this to manage the driver.
struct DriverLifecycle {
  // This is the version of `DriverLifecycle` and all structures used by it.
  // The version number will indicate which of the internal structs are available.
  // DRIVER_LIFECYCLE_VERSION_ macros below indicate the available version numbers.
  uint64_t version;

  struct v1 {
    // Pointer to a function that can start execution of the driver. This
    // function is executed on the default driver dispatcher that is passed in.
    // This dispatcher is synchronized and runs on a shared driver thread within a `driver_host`.
    // This function must allocate the driver structure on the heap and
    // store an opaque pointer to it in the |driver| parameter.
    // See
    // https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads#lifetime-dispatcher
    // for more information about the lifetime of a driver dispatcher.
    //
    // |start_args| contains the arguments for starting a driver.
    // |dispatcher| is the default synchronized fdf dispatcher on which to run the driver.
    // The driver is free to ignore this and use its own.
    // |driver| provides a place to store the opaque driver structure.
    zx_status_t (*start)(EncodedDriverStartArgs start_args, fdf_dispatcher_t* dispatcher,
                         void** driver);

    // Pointer to a function that can stop execution of the driver. This function
    // is executed after all dispatchers belonging to the driver have been shutdown.
    // It runs on a shared driver thread within a `driver_host`. The given |driver|
    // must get deallocated as part of this function.
    //
    // |driver| is the opaque driver structure that was stored as part of |start|.
    zx_status_t (*stop)(void* driver);
  } v1;
  struct v2 {
    // Pointer to a function that is triggered before the stop hook is invoked. This method allows
    // the driver to asynchronously do work that may not otherwise be done synchronously during the
    // stop hook.
    //
    // |driver| is the value that was stored when the driver was started.
    // |complete| is the callback that must be called to complete the |prepare_stop| operation.
    // |complete_cookie| is an opaque pointer provided by the caller (the Driver Framework), and
    // must be provided without modification into the |complete| callback function.
    void (*prepare_stop)(void* driver, PrepareStopCompleteCallback* complete,
                         void* complete_cookie);
  } v2;
  struct v3 {
    // Pointer to a function that can start execution of the driver. This
    // function is executed on the default driver dispatcher that is passed in.
    // This dispatcher is synchronized and runs on a shared driver thread within a `driver_host`.
    // This function must allocate the driver structure on the heap and
    // store an opaque pointer to it in the |driver| parameter.
    //
    // If available, preferred over `v1.start`.
    //
    // See
    // https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads#lifetime-dispatcher
    // for more information about the lifetime of a driver dispatcher.
    //
    // |start_args| contains the arguments for starting a driver.
    // |dispatcher| is the default synchronized fdf dispatcher on which to run the driver.
    // The driver is free to ignore this and use its own.
    // |complete| is the callback that must be called to complete the |start| operation.
    // |complete_cookie| is an opaque pointer provided by the caller (the Driver Framework), and
    // must be provided without modification into the |complete| callback function.
    void (*start)(EncodedDriverStartArgs start_args, fdf_dispatcher_t* dispatcher,
                  StartCompleteCallback* complete, void* complete_cookie);
  } v3;
};

#define DRIVER_LIFECYCLE_VERSION_1 1

#define DRIVER_LIFECYCLE_VERSION_2 2

#define DRIVER_LIFECYCLE_VERSION_3 3

#define DRIVER_LIFECYCLE_VERSION_MAX DRIVER_LIFECYCLE_VERSION_3

#define FUCHSIA_DRIVER_LIFECYCLE_V1(start, stop)                                 \
  extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__ __EXPORT {       \
    .version = DRIVER_LIFECYCLE_VERSION_1, .v1 = {start, stop}, .v2 = {nullptr}, \
  }

#define FUCHSIA_DRIVER_LIFECYCLE_V2(start, prepare_stop, stop)                        \
  extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__ __EXPORT {            \
    .version = DRIVER_LIFECYCLE_VERSION_2, .v1 = {start, stop}, .v2 = {prepare_stop}, \
  }

#define FUCHSIA_DRIVER_LIFECYCLE_V3(start2, prepare_stop, stop)                                  \
  extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__ __EXPORT {                       \
    .version = DRIVER_LIFECYCLE_VERSION_3, .v1 = {.start = nullptr, stop}, .v2 = {prepare_stop}, \
    .v3 = {start2},                                                                              \
  }

#endif  // LIB_DRIVER_SYMBOLS_SYMBOLS_H_
