// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_DRIVER_EXPORT_H_
#define LIB_DRIVER_COMPONENT_CPP_DRIVER_EXPORT_H_

#include <lib/driver/component/cpp/internal/lifecycle.h>

// The given |driver| needs to be a subclass of |fdf::DriverBase|.
// It must have a constructor in the form of:
// `T(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);`
#define FUCHSIA_DRIVER_EXPORT(driver) \
  FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf_internal::Lifecycle<driver>)

#endif  // LIB_DRIVER_COMPONENT_CPP_DRIVER_EXPORT_H_
