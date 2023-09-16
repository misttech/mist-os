// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_DRIVER_EXPORT_H_
#define LIB_DRIVER_COMPONENT_CPP_DRIVER_EXPORT_H_

#include <zircon/availability.h>

#if __Fuchsia_API_level__ >= 13

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
#include <lib/driver/component/cpp/internal/driver_server.h>
#else
#include <lib/driver/component/cpp/internal/lifecycle.h>
#endif

// The given |driver| needs to be a subclass of |fdf::DriverBase|.
// It must have a constructor in the form of:
// `T(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);`
// This MUST only be called once inside of a shared object, and it must be called from the root
// namespace and not nested inside any other namespace.
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
#define FUCHSIA_DRIVER_EXPORT(driver)                                                   \
  EXPORT_FUCHSIA_DRIVER_REGISTRATION_V1(fdf_internal::DriverServer<driver>::initialize, \
                                        fdf_internal::DriverServer<driver>::destroy)
#else
#define FUCHSIA_DRIVER_EXPORT(driver) \
  FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf_internal::Lifecycle<driver>)
#endif

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_DRIVER_EXPORT_H_
