/*
 * Copyright 2024 The Fuchsia Authors
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */
package android.system.microfuchsia.vm_service;

import android.system.microfuchsia.vm_service.IHostProxy;

/** {@hide} */
// This is the protocol used to communicate with the microfuchsia VM.
interface IMicrofuchsia {
  const int GUEST_PORT = 5680;

  void setHostProxy(IHostProxy proxy);

  // TODO: Add API to instantiate sessions with specific TAs.
}
