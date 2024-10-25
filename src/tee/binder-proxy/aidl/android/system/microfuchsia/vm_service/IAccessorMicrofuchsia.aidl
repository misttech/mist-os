/*
 * Copyright 2024 The Fuchsia Authors
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */
package android.system.microfuchsia.vm_service;

/** {@hide} */
// This is the protocol used to communicate with the microfuchsia VM.
interface IAccessorMicrofuchsia {
  const int GUEST_PORT = 5680;

  // TODO: Add APIs to communicate with TAs running inside microfuchsia.
}
