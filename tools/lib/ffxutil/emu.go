// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffxutil

import (
	"context"
	"fmt"
	"os/exec"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
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
	UEFI     string
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

type EmuStartArgs struct {
	// The emulation engine to run with. The currently supported values are
	// `aemu`, `qemu`, and `crosvm`. If empty, it will default to `aemu`.
	Engine string

	// If using a custom config, all other fields below should be empty.
	Config string

	// TODO(https://fxbug.dev/329144967): Add more fields as necessary
	// to provide as flags to `ffx emu start`.
	ProductBundle string
	KernelArgs    []string
	Accel         string
	Device        string
}

// EmuStartConsole returns a command to launch the emulator.
func (f *FFXInstance) EmuStartConsole(ctx context.Context, sdkRoot, name string, tools EmuTools, startArgs EmuStartArgs) (*exec.Cmd, error) {
	// If using different tools from the ones in the sdk, they need to be
	// provided in the sdk.overrides in the ffx config.
	toolsToOverride := make(map[string]string)
	if tools.Emulator != "" {
		expectedName := "aemu_internal"
		if startArgs.Engine != "" {
			expectedName = fmt.Sprintf("%s_internal", startArgs.Engine)
		}
		toolsToOverride[expectedName] = tools.Emulator
	}
	if tools.FVM != "" {
		toolsToOverride["fvm"] = tools.FVM
	}
	if tools.ZBI != "" {
		toolsToOverride["zbi"] = tools.ZBI
	}
	if tools.UEFI != "" {
		toolsToOverride["uefi_internal"] = tools.UEFI
	}
	for toolName, toolPath := range toolsToOverride {
		var err error
		toolPath, err = filepath.Abs(toolPath)
		if err != nil {
			return nil, err
		}
		if err := f.ConfigSet(ctx, fmt.Sprintf("sdk.overrides.%s", toolName), toolPath); err != nil {
			return nil, err
		}
	}
	absPath, err := filepath.Abs(sdkRoot)
	if err != nil {
		return nil, err
	}
	if err := f.ConfigSet(ctx, "sdk.root", absPath); err != nil {
		return nil, err
	}
	args := []string{"emu", "start", "--console", "--net", "tap", "--name", name, "-H", "-s", "0"}
	if startArgs.Config != "" {
		args = append(args, "--config", startArgs.Config)
	} else {
		args = append(args, startArgs.ProductBundle)
		for _, k := range startArgs.KernelArgs {
			args = append(args, "--kernel-args", k)
		}
		if startArgs.Accel != "" {
			args = append(args, "--accel", startArgs.Accel)
		}
		if startArgs.Device != "" {
			args = append(args, "--device", startArgs.Device)
		}
	}
	if startArgs.Engine != "" {
		// For `ffx emu start`, the `aemu` engine is specified as `femu`.
		engine := startArgs.Engine
		if engine == "aemu" {
			engine = "femu"
		}
		args = append(args, "--engine", engine)
	}
	dryRunCommand := append(args, "--dry-run")
	if err := f.Run(ctx, dryRunCommand...); err != nil {
		logger.Debugf(ctx, "failed to run dry-run command: %s", err)
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
