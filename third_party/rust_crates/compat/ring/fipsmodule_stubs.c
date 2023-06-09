// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#include "../crypto/fipsmodule/bn/internal.h"
#include "../crypto/fipsmodule/ec/ecp_nistz256.h"
#include "../crypto/limbs/limbs.h"

// This file provides stub implementations of functions implemented in assembly
// for platforms that are not supported. Using this file allows the build to
// link, but any call to these functions will most likely result in a crash.

extern void GFp_bn_mul_mont(BN_ULONG *rp, const BN_ULONG *ap, const BN_ULONG *bp,
                     const BN_ULONG *np, const BN_ULONG *n0, size_t num) {
  abort();
}
extern void GFp_nistz256_mul_mont(Limb res[P256_LIMBS], const Limb a[P256_LIMBS],
                           const Limb b[P256_LIMBS]) {
  abort();
}
extern void GFp_nistz256_sqr_mont(Limb res[P256_LIMBS], const Limb a[P256_LIMBS]) {
  abort();
}
extern void GFp_nistz256_neg(Limb res[P256_LIMBS], const Limb a[P256_LIMBS]) {
  abort();
}
extern void GFp_nistz256_point_double(P256_POINT *r, const P256_POINT *a) {
  abort();
}
extern void GFp_nistz256_point_add_affine(P256_POINT *r, const P256_POINT *a,
                                   const P256_POINT_AFFINE *b) {
  abort();
}

// The following functions do not have signatures defined.
// The parameters and return types are incorrect, but these defintions satisfy
// the linker.
extern void GFp_aes_hw_set_encrypt_key(void) {
  abort();
}
extern void GFp_aes_nohw_set_encrypt_key(void) {
  abort();
}
extern void GFp_aes_hw_encrypt(void) {
  abort();
}
extern void GFp_aes_nohw_encrypt(void) {
  abort();
}
extern void GFp_aes_hw_ctr32_encrypt_blocks(void) {
  abort();
}
extern void GFp_ChaCha20_ctr32(void) {
  abort();
}
extern void GFp_gcm_init_clmul(void) {
  abort();
}
extern void GFp_gcm_ghash_clmul(void) {
  abort();
}
extern void GFp_gcm_gmult_clmul(void) {
  abort();
}
extern void GFp_poly1305_blocks(void) {
  abort();
}
extern void GFp_poly1305_emit(void) {
  abort();
}
extern void GFp_poly1305_init_asm(void) {
  abort();
}
extern void GFp_nistz256_add(void) {
  abort();
}
