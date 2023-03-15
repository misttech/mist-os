// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PLATFORM_INCLUDE_PLATFORM_LEGACY_DEBUG_H_
#define ZIRCON_KERNEL_PLATFORM_INCLUDE_PLATFORM_LEGACY_DEBUG_H_

#include <stdlib.h>
#include <zircon/compiler.h>

// Platform specific debug port routing delegation. That is,
// when not using new uart implementations, calls will be delegated
// to legacy implementations.
//
// In order to use a new uart implementation:
// - gBootOptions->experimental_serial_migration must be true.
//
// TODO(fxbug.dev/89182): These calls and related code, will be deleted once migration is finalized.
void legacy_platform_dputs_thread(const char *str, size_t len);
void legacy_platform_dputs_irq(const char *str, size_t len);
int legacy_platform_dgetc(char *c, bool wait);

int legacy_platform_pgetc(char *c);
void legacy_platform_pputc(char c);

#endif  // ZIRCON_KERNEL_PLATFORM_INCLUDE_PLATFORM_LEGACY_DEBUG_H_
