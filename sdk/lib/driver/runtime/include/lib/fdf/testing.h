// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_RUNTIME_INCLUDE_LIB_FDF_TESTING_H_
#define LIB_DRIVER_RUNTIME_INCLUDE_LIB_FDF_TESTING_H_

#include <lib/fdf/dispatcher.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Run all of the driver runtime's dispatchers until everything is idle.
// This will return ZX_ERR_BAD_STATE if the driver runtime is managing any threads.
zx_status_t fdf_testing_run_until_idle();

// Adds |driver| to the thread's current call stack.
void fdf_testing_push_driver(const void* driver);

// Removes the driver at the top of the thread's current call stack.
void fdf_testing_pop_driver(void);

// Blocks the current thread until each runtime dispatcher in the process
// is observed to enter an idle state. This does not guarantee that all the
// dispatchers will be idle when this function returns. This will only wait
// on dispatchers that existed when this function was called. This does not
// include any new dispatchers that might have been created while the waiting
// was happening.
// This does not wait for registered waits that have not yet been signaled,
// or delayed tasks which have been scheduled for a future deadline.
// This should not be called from a thread managed by the driver runtime,
// such as from tasks or ChannelRead callbacks.
void fdf_testing_wait_until_all_dispatchers_idle(void);

// Blocks the current thread until each runtime dispatcher in the process
// is observed to have been destroyed.
//
// # Thread requirements
//
// This should not be called from a thread managed by the driver runtime,
// such as from tasks or ChannelRead callbacks.
void fdf_testing_wait_until_all_dispatchers_destroyed(void);

// Sets the default driver dispatcher to be returned when the current thread does not have a driver
// associated with it. This is useful for tests that want to attach a dispatcher to their main
// test thread, so that they can use a synchronization_checker without posting tasks manually.
// The dispatcher input should have been created using
// |fdf_env_dispatcher_create_with_owner|.
// This will return ZX_ERR_BAD_STATE if the driver runtime is managing any threads.
zx_status_t fdf_testing_set_default_dispatcher(fdf_dispatcher_t* dispatcher);

__END_CDECLS

#endif  // LIB_DRIVER_RUNTIME_INCLUDE_LIB_FDF_TESTING_H_
