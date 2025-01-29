// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffxutil

import (
	"context"
)

// Flash flashes the target.
func (f *FFXInstance) Flash(ctx context.Context, target, sshKey, productBundle string, tcp bool) error {
	if err := f.ConfigSet(ctx, "fastboot.flash.timeout_rate", "4"); err != nil {
		return err
	}
	ffxArgs := []string{"-v", "--target", target,
		"--config", "{\"ffx\": {\"fastboot\": {\"inline_target\": true}}}",
		"target", "flash"}
	if sshKey != "" {
		ffxArgs = append(ffxArgs, "--authorized-keys", sshKey)
	}

	ffxArgs = append(ffxArgs, "--product-bundle", productBundle)
	if tcp {
		// Rebooting while flashing over TCP will error out.
		ffxArgs = append(ffxArgs, "--no-bootloader-reboot")
	}
	return f.RunWithTimeout(ctx, 0, ffxArgs...)
}
