/*
 * Copyright 2024 The Fuchsia Authors
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */
package android.system.microfuchsia.trusted_app;

/** {@hide} */
// Connection to a with a TA
interface ITrustedApp {
  // TODO: Accept a parameter set and return a result + new session instance.
  void openSession();
}
