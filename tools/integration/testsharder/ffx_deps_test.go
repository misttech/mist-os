// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"fmt"
	"os"
	"path/filepath"
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
			want:       append(baseDeps, "host_x64/ffx-test", "host_x64/ffx-test.json", "host_x64/ffx-emu", "host_x64/ffx-emu.json"),
		},
		{
			name:       "NUC bootloader boot deps",
			targetCPU:  "x64",
			deviceType: "NUC",
			want:       []string{"zircon-a.zbi", "zircon-a.vbmeta", "host_x64/ffx-test", "host_x64/ffx-test.json"},
		},
		{
			name:       "AEMU x64 deps",
			targetCPU:  "x64",
			deviceType: "AEMU",
			want:       append(baseDeps, "host_x64/ffx-test", "host_x64/ffx-test.json", "host_x64/ffx-emu", "host_x64/ffx-emu.json"),
		},
		{
			name:       "QEMU arm64 deps",
			targetCPU:  "arm64",
			deviceType: "QEMU",
			want:       append(baseDeps, "host_arm64/ffx-emu", "host_arm64/ffx-emu.json"),
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
			for cpu, subtools := range map[string][]string{"x64": {"ffx-test", "ffx-emu"}, "arm64": {"ffx-emu"}} {
				subtoolDir := filepath.Join(buildDir, fmt.Sprintf("host_%s", cpu))
				if err := os.MkdirAll(subtoolDir, os.ModePerm); err != nil {
					t.Fatalf("failed to mkdirAll %s: %s", subtoolDir, err)
				}
				for _, subtool := range subtools {
					subtoolPath := filepath.Join(subtoolDir, subtool)
					subtoolJsonPath := fmt.Sprintf("%s.json", subtoolPath)
					if err := os.WriteFile(subtoolPath, []byte{}, os.ModePerm); err != nil {
						t.Fatalf("failed to write subtool at %s: %s", subtoolPath, err)
					}
					if err := os.WriteFile(subtoolJsonPath, []byte{}, os.ModePerm); err != nil {
						t.Fatalf("failed to write subtool json file at %s: %s", subtoolJsonPath, err)
					}
				}
			}
			s := &Shard{
				Env: build.Environment{
					Dimensions: build.DimensionSet{
						"device_type": tc.deviceType,
					},
				},
				Tests: []Test{{Test: build.Test{CPU: tc.targetCPU}}},
			}
			if err := AddFFXDeps(s, buildDir, []build.Image{
				{Name: "zircon-a", Path: "zircon-a.zbi", Type: "zbi"},
				{Name: "zircon-a", Path: "zircon-a.vbmeta", Type: "vbmeta"},
				{Name: "fuchsia", Path: "fuchsia.zbi", Type: "zbi"},
			}, build.Tools{
				{Name: "ffx-test", OS: "linux", CPU: "x64", Path: "host_x64/ffx-test"},
				{Name: "ffx-emu", OS: "linux", CPU: "x64", Path: "host_x64/ffx-emu"},
				{Name: "ffx-emu", OS: "linux", CPU: "arm64", Path: "host_arm64/ffx-emu"},
				{Name: "ffx-product", OS: "linux", CPU: "x64", Path: "host_x64/ffx-product"},
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
