// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

// for testability
type FFXInterface interface {
	Run(context.Context, ...string) error
	GetPBArtifacts(context.Context, string, string) ([]string, error)
	Stop() error
}

var GetFFX = func(ctx context.Context, ffxPath, outputsDir string) (FFXInterface, error) {
	return ffxutil.NewFFXInstance(ctx, ffxPath, "", []string{}, "", "", outputsDir, ffxutil.UseFFXLegacy)
}

// AddImageDeps selects and adds the subset of images needed by a shard to
// that shard's list of dependencies.
func AddImageDeps(ctx context.Context, s *Shard, buildDir string, images []build.Image, pave bool, pbPath, ffxPath string) error {
	// Host-test only shards do not require any image deps because they are not running
	// against a Fuchsia target.
	if s.Env.Dimensions.DeviceType() == "" {
		return nil
	}
	imageDeps := []string{"images.json"}
	// GCE test shards do not require any image deps as the build creates a
	// compute image with all the deps baked in.
	if s.Env.Dimensions.DeviceType() == "GCE" {
		s.AddDeps(imageDeps)
		return nil
	}

	// TODO(https://fxbug.dev/42083611): Remove these images when product bundles are used as soon
	// as ffx emu and flash are enabled by default. Otherwise we need to provide
	// images from both images.json and product_bundles.json since either can be used
	// depending on what ffx experiment level is being run and whether the device that's
	// being targeted is idling in fastboot or not.
	for _, image := range images {
		if isUsedForTesting(s, image, pave) {
			if _, err := os.Stat(filepath.Join(buildDir, image.Path)); err != nil {
				if !errors.Is(err, os.ErrNotExist) {
					return err
				}
			} else {
				imageDeps = append(imageDeps, image.Path)
			}
		}
	}

	// Add product bundle related artifacts.
	imageDeps = append(imageDeps, "product_bundles.json")

	tmp, err := os.MkdirTemp("", "wt")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	ffxOutputsDir := filepath.Join(tmp, "ffx_outputs")
	ffx, err := GetFFX(ctx, ffxPath, ffxOutputsDir)
	if err != nil {
		return err
	}
	if ffx == nil {
		return fmt.Errorf("failed to initialize an ffx instance")
	}
	defer func() {
		if err := ffx.Stop(); err != nil {
			logger.Debugf(ctx, "failed to stop ffx: %s", err)
		}
	}()

	if err := ffx.Run(ctx, "config", "set", "daemon.autostart", "false"); err != nil {
		return err
	}
	artifactsGroup := "flash"
	if s.Env.TargetsEmulator() {
		artifactsGroup = "emu"
	}
	artifacts, err := ffx.GetPBArtifacts(ctx, filepath.Join(buildDir, pbPath), artifactsGroup)
	if err != nil {
		return err
	}
	for _, a := range artifacts {
		imageDeps = append(imageDeps, filepath.Join(pbPath, a))
	}
	bootloaderArtifacts, err := ffx.GetPBArtifacts(ctx, filepath.Join(buildDir, pbPath), "bootloader")
	if err != nil {
		return err
	}
	for _, a := range bootloaderArtifacts {
		parts := strings.SplitN(a, ":", 2)
		if parts[0] == "firmware_fat" {
			imageDeps = append(imageDeps, filepath.Join(pbPath, parts[1]))
		}
	}

	s.AddDeps(imageDeps)
	return nil
}

func isUsedForTesting(s *Shard, image build.Image, pave bool) bool {
	if s.Env.TargetsEmulator() {
		// All EMU targets in botanist are using product bundles, so we don't
		// need to get any deps from the images.json.
		return false
	}
	if isFlashingDep(image) {
		return true
	}
	// TODO(https://fxbug.dev/42124288): Remove zedboot/paving images once we switch to flashing.
	return ((pave && len(image.PaveArgs) != 0) ||
		(!pave && len(image.NetbootArgs) != 0) ||
		(len(image.PaveZedbootArgs) != 0) ||
		(pave && len(image.FastbootFlashArgs) != 0) ||
		(!pave && len(image.FastbootBootArgs) != 0))
}

func isFlashingDep(image build.Image) bool {
	return image.Name == "fastboot"
}
