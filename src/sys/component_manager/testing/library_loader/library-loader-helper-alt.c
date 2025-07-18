// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sys/component_manager/testing/library_loader/library-loader-helper.h"

// This is an alternate version of `library-loader-helper.c` that sets this
// variable to 43 instead of 42.
__attribute__((visibility("default"))) const int helper_value = 43;
