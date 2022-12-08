// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffxutil

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
)

const (
	// The path to the SDK manifest relative to the sdk.root.
	SDKManifestPath = "sdk/manifest/core"
)

// EmuTools represent tools used by `ffx emu`. If using tools not included in the SDK,
// their paths should be provided in this struct to EmuStart().
type EmuTools struct {
	Emulator string
	FVM      string
	ZBI      string
}

// SDKManifest contains the atoms that are part of the "SDK" which ffx looks up to find
// the tools it needs to launch an emulator. The manifest should only contain references
// to files that exist.
type SDKManifest struct {
	Atoms []Atom `json:"atoms"`
}

type Atom struct {
	Files []File `json:"files"`
	ID    string `json:"id"`
	Meta  string `json:"meta"`
}

type File struct {
	Destination string `json:"destination"`
	Source      string `json:"source"`
}

// EmuStartConsole returns a command to launch the emulator.
func (f *FFXInstance) EmuStartConsole(ctx context.Context, sdkRoot, name string, qemu bool, config string, tools EmuTools) (*exec.Cmd, error) {
	// If using different tools from the ones in the sdk, `ffx emu` expects them to
	// have certain names and to be located in a parent directory of the ffx binary.
	ffxDir := filepath.Dir(f.ffxPath)
	toolsToSymlink := make(map[string]string)
	if tools.Emulator != "" {
		expectedName := "aemu_internal"
		if qemu {
			expectedName = "qemu_internal"
		}
		toolsToSymlink[tools.Emulator] = filepath.Join(ffxDir, expectedName)
	}
	if tools.FVM != "" {
		toolsToSymlink[tools.FVM] = filepath.Join(ffxDir, "fvm")
	}
	if tools.ZBI != "" {
		toolsToSymlink[tools.ZBI] = filepath.Join(ffxDir, "zbi")
	}
	for oldname, newname := range toolsToSymlink {
		if oldname == newname {
			continue
		}
		if err := os.Symlink(oldname, newname); err != nil && !os.IsExist(err) {
			return nil, err
		}
	}
	if err := f.ConfigSet(ctx, "sdk.type", "in-tree"); err != nil {
		return nil, err
	}
	absPath, err := filepath.Abs(sdkRoot)
	if err != nil {
		return nil, err
	}
	if err := f.ConfigSet(ctx, "sdk.root", absPath); err != nil {
		return nil, err
	}
	args := []string{"emu", "start", "--console", "--net", "tap", "--name", name, "-H", "-s", "0", "--config", config}
	if qemu {
		args = append(args, "--engine", "qemu")
	}
	return f.Command(args...), nil
}

// EmuStop terminates all emulator instances launched by ffx.
func (f *FFXInstance) EmuStop(ctx context.Context) error {
	return f.Run(ctx, "emu", "stop", "--all")
}

// GetEmuDeps returns the list of file dependencies for `ffx emu` to work.
func GetEmuDeps(sdkRoot string, targetCPU string, tools []string) ([]string, error) {
	deps := []string{
		SDKManifestPath,
	}

	manifestPath := filepath.Join(sdkRoot, SDKManifestPath)
	manifest, err := GetFFXEmuManifest(manifestPath, targetCPU, tools)
	if err != nil {
		return nil, err
	}

	for _, atom := range manifest.Atoms {
		for _, file := range atom.Files {
			deps = append(deps, file.Source)
		}
	}
	return deps, nil
}

// GetFFXEmuManifest returns an SDK manifest with the minimum number of atoms
// required by `ffx emu`. The `tools` are the names of the tools that we expect to
// use from the SDK.
func GetFFXEmuManifest(manifestPath, targetCPU string, tools []string) (SDKManifest, error) {
	var manifest SDKManifest
	if err := jsonutil.ReadFromFile(manifestPath, &manifest); err != nil {
		return manifest, fmt.Errorf("failed to read sdk manifest: %w", err)
	}
	if len(tools) == 0 {
		manifest.Atoms = []Atom{}
		return manifest, nil
	}

	toolIds := make(map[string]struct{})
	for _, tool := range tools {
		toolIds[fmt.Sprintf("sdk://tools/%s/%s", targetCPU, tool)] = struct{}{}
	}

	requiredAtoms := []Atom{}
	for _, atom := range manifest.Atoms {
		if _, ok := toolIds[atom.ID]; !ok {
			continue
		}
		requiredAtoms = append(requiredAtoms, atom)
	}
	manifest.Atoms = requiredAtoms
	return manifest, nil
}
