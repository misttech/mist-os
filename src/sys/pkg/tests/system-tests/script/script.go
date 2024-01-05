// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package script

import (
	"context"
	"fmt"
	"os"
	"os/exec"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

// RunScript runs a script on the device and returns the result.
func RunScript(
	ctx context.Context,
	device *device.Client,
	repo *packages.Repository,
	script string,
) error {
	if script == "" {
		return nil
	}

	logger.Debugf(ctx, "running script: %s", script)
	cmd := exec.CommandContext(ctx, "/bin/sh", "-c", script)
	cmd.Env = os.Environ()
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr

	if err := cmd.Run(); err != nil {
		return fmt.Errorf("script %v failed to run: %w", script, err)
	}

	return nil
}
