// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffx

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type FFXTool struct {
	ffxToolPath string
	isolateDir  string
	stdout      io.Writer
}

func NewFFXTool(ffxToolPath string) (*FFXTool, error) {
	return NewFFXToolWithStdout(ffxToolPath, nil)
}

func NewFFXToolWithStdout(ffxToolPath string, stdout io.Writer) (*FFXTool, error) {
	isolateDir, err := os.MkdirTemp("", "systemTestIsoDir*")
	if err != nil {
		return nil, err
	}
	if _, err := os.Stat(ffxToolPath); err != nil {
		return nil, fmt.Errorf("error accessing %v: %w", ffxToolPath, err)
	}
	return &FFXTool{
		ffxToolPath: ffxToolPath,
		isolateDir:  isolateDir,
		stdout:      stdout,
	}, nil
}

func (f *FFXTool) SetStdout(stdout io.Writer) {
	f.stdout = stdout
}

func (f *FFXTool) Close() error {
	return os.RemoveAll(f.isolateDir)
}

type targetEntry struct {
	NodeName    string   `json:"nodename"`
	Addresses   []string `json:"addresses"`
	TargetState string   `json:"target_state"`
}

func (f *FFXTool) TargetList(ctx context.Context) ([]targetEntry, error) {
	args := []string{
		"--machine",
		"json",
		"target",
		"list",
	}

	stdout, stderr, err := util.RunCommand(ctx, f.ffxToolPath, args...)
	if err != nil {
		return []targetEntry{}, fmt.Errorf("ffx target list failed: %w: %s", err, string(stderr))
	}

	if len(stdout) == 0 {
		return []targetEntry{}, nil
	}

	var entries []targetEntry
	if err := json.Unmarshal(stdout, &entries); err != nil {
		return []targetEntry{}, err
	}

	return entries, nil
}

func (f *FFXTool) TargetListForNode(ctx context.Context, nodeNames []string) ([]targetEntry, error) {
	entries, err := f.TargetList(ctx)
	if err != nil {
		return []targetEntry{}, err
	}

	if len(nodeNames) == 0 {
		return entries, nil
	}

	var matchingTargets []targetEntry

	for _, target := range entries {
		for _, nodeName := range nodeNames {
			if target.NodeName == nodeName {
				matchingTargets = append(matchingTargets, target)
			}
		}
	}

	return matchingTargets, nil
}

func (f *FFXTool) SupportsZedbootDiscovery(ctx context.Context) (bool, error) {
	// Check if ffx is configured to resolve devices in zedboot.
	args := []string{
		"config",
		"get",
		"discovery.zedboot.enabled",
	}

	stdout, stderr, err := util.RunCommand(ctx, f.ffxToolPath, args...)
	if err != nil {
		// `ffx config get` exits with 2 if variable is undefined.
		if exiterr, ok := err.(*exec.ExitError); ok {
			if exiterr.ExitCode() == 2 {
				return false, nil
			}
		}

		return false, fmt.Errorf("ffx config get failed: %w: %s", err, string(stderr))
	}

	// FIXME(fxbug.dev/109280): Unfortunately we need to parse the raw string to see if it's true.
	if string(stdout) == "true\n" {
		return true, nil
	}

	return false, nil
}

func (f *FFXTool) TargetAdd(ctx context.Context, target string) error {
	args := []string{"target", "add", "--nowait", target}
	return f.runFFXCmd(ctx, args...)
}

func (f *FFXTool) Flash(ctx context.Context, target, FlashManifest string, args ...string) error {
	var finalArgs []string
	if target != "" {
		finalArgs = []string{"--target", target}
	}
	finalArgs = append(finalArgs, []string{"target", "flash"}...)
	finalArgs = append(finalArgs, args...)
	finalArgs = append(finalArgs, FlashManifest)
	return f.runFFXCmd(ctx, finalArgs...)
}

func (f *FFXTool) runFFXCmd(ctx context.Context, args ...string) error {
	path, err := exec.LookPath(f.ffxToolPath)
	if err != nil {
		return err
	}
	// prepend a config flag for finding subtools that are compiled separately
	// in the same directory as ffx itself.
	args = append([]string{"--config", fmt.Sprintf("ffx.subtool-search-paths=%s", filepath.Dir(path))}, args...)
	logger.Infof(ctx, "running: %s %q", path, args)
	cmd := exec.CommandContext(ctx, path, args...)
	if f.stdout != nil {
		cmd.Stdout = f.stdout
	} else {
		cmd.Stdout = os.Stdout
	}
	cmd.Stderr = os.Stderr
	cmd.Env = os.Environ()
	cmd.Env = append(cmd.Env, fmt.Sprintf("FFX_ISOLATE_DIR=%s", f.isolateDir))

	cmdRet := cmd.Run()
	logger.Infof(ctx, "finished running %s %q: %q", path, args, cmdRet)
	return cmdRet
}

func (f *FFXTool) RepositoryCreate(ctx context.Context, repoDir, keysDir string) error {
	args := []string{
		"--config", "ffx_repository=true",
		"repository",
		"create",
		"--keys", keysDir,
		repoDir,
	}

	return f.runFFXCmd(ctx, args...)
}

func (f *FFXTool) RepositoryPublish(ctx context.Context, repoDir string, packageManifests []string, additionalArgs ...string) error {
	args := []string{
		"repository",
		"publish",
	}

	for _, manifest := range packageManifests {
		args = append(args, "--package", manifest)
	}

	args = append(args, additionalArgs...)
	args = append(args, repoDir)

	return f.runFFXCmd(ctx, args...)
}
