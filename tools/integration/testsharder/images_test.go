// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"context"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
)

func mockImages(t *testing.T) ([]build.Image, string) {
	t.Helper()
	imgs := []build.Image{
		{
			PaveArgs: []string{"--boot", "--zircona"},
			Name:     "zircon-a",
			Path:     "fuchsia.zbi",
			Label:    "//build/images:fuchsia-zbi",
			Type:     "zbi",
		},
		{
			PaveZedbootArgs: []string{"--zirconr"},
			Name:            "zircon-r",
			Path:            "zedboot.zbi",
			Label:           "//build/images:zedboot-zbi",
			Type:            "zbi",
		},
		{
			NetbootArgs: []string{"--boot"},
			Name:        "netboot",
			Path:        "netboot.zbi",
			Label:       "//build/images:netboot-zbi",
			Type:        "zbi",
		},
		{
			Name:  "qemu-kernel",
			Path:  "multiboot.bin",
			Label: "//build/images:qemu-kernel",
			Type:  "kernel",
		},
		{
			Name:  "storage-full",
			Path:  "obj/build/images/fuchsia/fuchsia/fvm.blk",
			Label: "//build/images/fuchsia/my-fvm",
			Type:  "blk",
		},
	}
	imgDir := t.TempDir()
	for _, img := range imgs {
		path := filepath.Join(imgDir, img.Path)
		dir := filepath.Dir(path)
		if err := os.MkdirAll(dir, os.ModePerm); err != nil {
			t.Fatalf("MkdirAll(%s) failed: %s", dir, err)
		}
		f, err := os.Create(path)
		if err != nil {
			t.Fatalf("Create(%s) failed: %s", path, err)
		}
		defer f.Close()
	}
	return imgs, imgDir
}

type mockFFX struct {
	ffxutil.MockFFXInstance
}

func (m *mockFFX) GetPBArtifacts(ctx context.Context, pbPath, group string) ([]string, error) {
	var artifacts []string
	if group == "bootloader" {
		if strings.Contains(pbPath, "efi") {
			artifacts = append(artifacts, "firmware_fat:efi")
		}
	} else {
		artifacts = append(artifacts, "zbi")
		if group == "emu" {
			artifacts = append(artifacts, "qemu-kernel")
		}
	}
	return artifacts, nil
}

func TestAddImageDeps(t *testing.T) {
	imgs, imgDir := mockImages(t)
	defaultDeps := func(pbPath string, emu bool) []string {
		deps := []string{"images.json", "product_bundles.json"}
		if pbPath == "" {
			pbPath = "obj/build/images/fuchsia/product_bundle"
		}
		deps = append(deps, filepath.Join(pbPath, "zbi"))
		if emu {
			deps = append(deps, filepath.Join(pbPath, "qemu-kernel"))
		}
		return deps
	}
	testCases := []struct {
		name       string
		pave       bool
		deviceType string
		pbPath     string
		want       []string
	}{
		{
			name:       "emulator image deps",
			deviceType: "AEMU",
			pave:       false,
			want:       defaultDeps("", true),
		},
		{
			name:       "paving image deps",
			deviceType: "NUC",
			pave:       true,
			want:       append(defaultDeps("", false), "fuchsia.zbi", "zedboot.zbi"),
		},
		{
			name:       "netboot image deps",
			deviceType: "NUC",
			pave:       false,
			want:       append(defaultDeps("", false), "netboot.zbi", "zedboot.zbi"),
		},
		{
			name:       "emulator env with efi",
			deviceType: "AEMU",
			pave:       false,
			pbPath:     "efi-boot-test/product_bundle",
			want:       append(defaultDeps("efi-boot-test/product_bundle", true), "efi-boot-test/product_bundle/efi"),
		},
		{
			name:       "GCE image deps",
			deviceType: "GCE",
			pave:       false,
			want:       []string{"images.json"},
		},
		{
			name: "host-test only shard image deps",
			pave: false,
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			s := &Shard{
				Env: build.Environment{
					Dimensions: build.DimensionSet{
						"device_type": tc.deviceType,
					},
				},
			}
			origGetFFX := GetFFX
			defer func() {
				GetFFX = origGetFFX
			}()
			GetFFX = func(ctx context.Context, ffxPath, outputsDir string) (FFXInterface, error) {
				return &mockFFX{}, nil
			}
			pbPath := "obj/build/images/fuchsia/product_bundle"
			if tc.pbPath != "" {
				pbPath = tc.pbPath
			}
			AddImageDeps(context.Background(), s, imgDir, imgs, tc.pave, pbPath, "path/to/ffx")
			sort.Strings(tc.want)
			sort.Strings(s.Deps)
			if diff := cmp.Diff(tc.want, s.Deps); diff != "" {
				t.Errorf("AddImageDeps(%v, %v, %t) failed: (-want +got): \n%s", s, imgs, tc.pave, diff)
			}
		})
	}
}
