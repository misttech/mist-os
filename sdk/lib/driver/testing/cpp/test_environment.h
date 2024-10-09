// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_
#define LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_

#include <zircon/availability.h>

#if FUCHSIA_API_LEVEL_LESS_THAN(24)
#include <lib/driver/testing/cpp/internal/test_environment.h>
#endif  // FUCHSIA_API_LEVEL_LESS_THAN(24)

#endif  // LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_
