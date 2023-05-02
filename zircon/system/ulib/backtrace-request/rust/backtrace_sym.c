// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/backtrace-request/backtrace-request.h>
#include <zircon/compiler.h>

// Offers symbols that can be called from rust code to generate backtrace requests.
void backtrace_request_all_threads_for_rust(void) { backtrace_request_all_threads(); }
void backtrace_request_current_thread_for_rust(void) { backtrace_request_current_thread(); }
