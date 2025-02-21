// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/counter_dispatcher.h"

#include <lib/counters.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/move.h>

#include <ktl/enforce.h>

KCOUNTER(dispatcher_counter_create_count, "dispatcher.counter.create")
KCOUNTER(dispatcher_counter_destroy_count, "dispatcher.counter.destroy")

// static
zx_status_t CounterDispatcher::Create(KernelHandle<CounterDispatcher>* handle,
                                      zx_rights_t* rights) {
  fbl::AllocChecker ac;
  KernelHandle counter(fbl::AdoptRef(new (&ac) CounterDispatcher));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *rights = default_rights();
  *handle = ktl::move(counter);
  return ZX_OK;
}

CounterDispatcher::CounterDispatcher() {
  UpdateState(0u, ZX_COUNTER_NON_POSITIVE);
  kcounter_add(dispatcher_counter_create_count, 1);
}

CounterDispatcher::~CounterDispatcher() { kcounter_add(dispatcher_counter_destroy_count, 1); }
