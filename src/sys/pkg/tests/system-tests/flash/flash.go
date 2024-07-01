// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package flash

import (
	"context"
	"fmt"
	"os"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"golang.org/x/crypto/ssh"
)

func FlashDevice(
	ctx context.Context,
	d *device.Client,
	ffx *ffx.FFXTool,
	build artifacts.Build,
	publicKey ssh.PublicKey,
) error {
	logger.Infof(ctx, "Starting to flash device")
	startTime := time.Now()

	if err := d.Flash(ctx, ffx, build, publicKey); err != nil {
		return fmt.Errorf("device failed to flash: %w", err)
	}

	if err := d.Reconnect(ctx, ffx); err != nil {
		return fmt.Errorf("device failed to connect after flash: %w", err)
	}

	logger.Infof(ctx, "device booted")
	logger.Infof(ctx, "Flashing successful in %s", time.Now().Sub(startTime))

	startTime = time.Now()
	cmd := []string{"/bin/update", "wait-for-commit"}
	if err := d.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
		return fmt.Errorf("update wait-for-commit failed after flash: %w", err)
	}
	logger.Infof(ctx, "Commit successful in %s", time.Now().Sub(startTime))

	return nil
}
