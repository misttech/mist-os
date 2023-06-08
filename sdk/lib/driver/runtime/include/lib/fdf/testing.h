// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_FDF_TESTING_H_
#define LIB_FDF_TESTING_H_

#include <lib/fdf/dispatcher.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Run the unmanaged testing dispatcher on the current thread until it is idle.
// If an unmanaged dispatcher has not been created, return ZX_ERR_BAD_STATE.
// See |fdf_testing_create_unmanaged_dispatcher| for creating the unmanaged dispatcher.
zx_status_t fdf_testing_run_until_idle();

// Sets the default driver dispatcher to be returned when the current thread does not have a driver
// associated with it. This is useful for tests that want to attach a dispatcher to their main
// test thread, so that they can use a synchronization_checker without posting tasks manually.
// The dispatcher input should have been created using
// |fdf_testing_create_unmanaged_dispatcher|.
// This will return ZX_ERR_BAD_STATE if called from a thread managed by the driver runtime.
zx_status_t fdf_testing_set_default_dispatcher(fdf_dispatcher_t* dispatcher);

// Creates an unmanaged testing dispatcher that can be manually run on the main test thread using
// |fdf_testing_run_until_idle|. This dispatcher is not ran on the runtime managed thread pools.
// See |fdf_env_dispatcher_create_with_owner| for more information about the parameters.
zx_status_t fdf_testing_create_unmanaged_dispatcher(const void* driver, uint32_t options,
                                                    const char* name, size_t name_len,
                                                    fdf_dispatcher_shutdown_observer_t* observer,
                                                    fdf_dispatcher_t** out_dispatcher);

__END_CDECLS

#endif  // LIB_FDF_TESTING_H_
