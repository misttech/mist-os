// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"fmt"
	"runtime"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/hostplatform"
)

// AddFFXDeps selects and adds the files needed by ffx to provision a device
// or launch an emulator to the shard's list of dependencies.
func AddFFXDeps(s *Shard, buildDir string, images []build.Image, tools build.Tools, flash bool) error {
	if len(s.Tests) == 0 {
		return fmt.Errorf("shard %s has no tests", s.Name)
	}
	subtools := []string{"test"}
	if s.Env.TargetsEmulator() {
		subtools = append(subtools, "emu", "product")
		deps, err := ffxutil.GetEmuDeps(buildDir, s.TargetCPU(), []string{})
		if err != nil {
			return err
		}
		s.AddDeps(deps)
	} else if flash && s.Env.Dimensions.DeviceType() != "" {
		deps, err := ffxutil.GetFlashDeps(buildDir, "fuchsia")
		if err != nil {
			return err
		}
		s.AddDeps(deps)
	} else if s.Env.Dimensions.DeviceType() != "" && s.ImageOverrides.IsEmpty() {
		deps := []string{}
		for _, image := range images {
			// This provisions the images used by `ffx target bootloader boot` in botanist:
			// https://cs.opensource.google/fuchsia/fuchsia/+/master:tools/botanist/targets/device.go?q=zircon-a
			if image.Name == "zircon-a" {
				deps = append(deps, image.Path)
			}
		}
		s.AddDeps(deps)
	}
	s.AddDeps(getSubtoolDeps(s, tools, buildDir, subtools))
	return nil
}

func getSubtoolDeps(s *Shard, tools build.Tools, buildDir string, subtools []string) []string {
	platform := hostplatform.MakeName(runtime.GOOS, s.HostCPU())
	var deps []string
	for _, s := range subtools {
		if subtool, err := tools.LookupTool(platform, fmt.Sprintf("ffx-%s", s)); err == nil {
			deps = append(deps, subtool.Path)
			deps = append(deps, subtool.RuntimeFiles...)
		}
	}
	return deps
}
