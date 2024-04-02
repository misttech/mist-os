// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <assert.h>
#include <lib/arch/intrin.h>
#include <lib/arch/x86/boot-cpuid.h>
#include <lib/fit/defer.h>
#include <trace.h>
#include <zircon/errors.h>

#include <arch/x86/feature.h>
#include <arch/x86/page_tables/constants.h>
#include <arch/x86/page_tables/page_tables.h>
#include <fbl/algorithm.h>
#include <ktl/algorithm.h>
#include <ktl/iterator.h>
#include <page_tables/x86/constants.h>
#include <vm/physmap.h>
#include <vm/pmm.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0
