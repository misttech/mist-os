// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffxutil

import (
	"context"
)

// Flash flashes the target.
func (f *FFXInstance) Flash(ctx context.Context, target, sshKey, productBundle string, tcp bool) error {
	configs := map[string]any{
		"discovery.mdns.enabled":      false,
		"fastboot.usb.disabled":       true,
		"discovery.timeout":           12000,
		"fastboot.flash.timeout_rate": "4",
	}

	args := []string{"-v", "target", "flash", "--product-bundle", productBundle}
	if tcp {
		// Rebooting while flashing over TCP will error out.
		args = append(args, "--no-bootloader-reboot")
	}
	if sshKey != "" {
		args = append(args, "--authorized-keys", sshKey)
	}
	return f.invoker(args).setConfigs(configs).setTarget(target).setStrict().setTimeout(0).run(ctx)
}
