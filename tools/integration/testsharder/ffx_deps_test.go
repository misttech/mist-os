// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"testing"

	"github.com/google/go-cmp/cmp"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
)

func TestAddFFXDeps(t *testing.T) {
	baseDeps := []string{
		"sdk/manifest/core",
	}
	testCases := []struct {
		name       string
		targetCPU  string
		deviceType string
		want       []string
	}{
		{
			name:       "QEMU x64 deps",
			targetCPU:  "x64",
			deviceType: "QEMU",
			want:       append(baseDeps, "host_x64/ffx", "host_x64/ffx-test", "host_x64/ffx-test.json", "host_x64/ffx-emu", "host_x64/ffx-emu.json"),
		},
		{
			name:       "NUC bootloader boot deps",
			targetCPU:  "x64",
			deviceType: "NUC",
			want:       []string{"host_x64/ffx", "host_x64/ffx-test", "host_x64/ffx-test.json"},
		},
		{
			name:       "AEMU x64 deps",
			targetCPU:  "x64",
			deviceType: "AEMU",
			want:       append(baseDeps, "host_x64/ffx", "host_x64/ffx-test", "host_x64/ffx-test.json", "host_x64/ffx-emu", "host_x64/ffx-emu.json"),
		},
		{
			name:       "QEMU arm64 deps",
			targetCPU:  "arm64",
			deviceType: "QEMU",
			want:       append(baseDeps, "host_arm64/ffx", "host_arm64/ffx-emu", "host_arm64/ffx-emu.json"),
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			buildDir := t.TempDir()
			manifestPath := filepath.Join(buildDir, ffxutil.SDKManifestPath)
			if err := os.MkdirAll(filepath.Dir(manifestPath), os.ModePerm); err != nil {
				t.Fatalf("failed to mkdirAll %s: %s", filepath.Dir(manifestPath), err)
			}
			if err := jsonutil.WriteToFile(manifestPath, ffxutil.SDKManifest{Atoms: []ffxutil.Atom{}}); err != nil {
				t.Fatalf("failed to write manifest at %s: %s", manifestPath, err)
			}
			s := &Shard{
				Env: build.Environment{
					Dimensions: build.DimensionSet{
						"device_type": tc.deviceType,
					},
				},
				Tests: []Test{{Test: build.Test{CPU: tc.targetCPU}}},
			}
			hostOS := runtime.GOOS
			if err := AddFFXDeps(s, buildDir, build.Tools{
				{Name: "ffx", OS: hostOS, CPU: "x64", Path: "host_x64/ffx"},
				{Name: "ffx", OS: hostOS, CPU: "arm64", Path: "host_arm64/ffx"},
				{Name: "ffx-test", OS: hostOS, CPU: "x64", Path: "host_x64/ffx-test",
					RuntimeFiles: []string{"host_x64/ffx-test.json"}},
				{Name: "ffx-emu", OS: hostOS, CPU: "x64", Path: "host_x64/ffx-emu",
					RuntimeFiles: []string{"host_x64/ffx-emu.json"}},
				{Name: "ffx-emu", OS: hostOS, CPU: "arm64", Path: "host_arm64/ffx-emu",
					RuntimeFiles: []string{"host_arm64/ffx-emu.json"}},
			}, false); err != nil {
				t.Errorf("failed to add ffx deps: %s", err)
			}
			sort.Strings(tc.want)
			if diff := cmp.Diff(tc.want, s.Deps); diff != "" {
				t.Errorf("AddFFXDeps(%v, %s) failed: (-want +got): \n%s", s, buildDir, diff)
			}
		})
	}
}
