// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package zbi

import (
	"context"
	"fmt"
	"io"
	"io/fs"
	"io/ioutil"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

const basePackagePrefix = "zircon.system.pkgfs.cmd=bin/pkgsvr+"

type ZBITool struct {
	zbiToolPath string
	stdout      io.Writer
}

func NewZBITool(zbiToolPath string) (*ZBITool, error) {
	return NewZBIToolWithStdout(zbiToolPath, nil)
}

func NewZBIToolWithStdout(zbiToolPath string, stdout io.Writer) (*ZBITool, error) {
	if _, err := os.Stat(zbiToolPath); err != nil {
		return nil, err
	}
	return &ZBITool{
		zbiToolPath: zbiToolPath,
		stdout:      stdout,
	}, nil
}

func (z *ZBITool) MakeImageArgsZbi(ctx context.Context, destPath string, imageArgs map[string]string) error {
	imageArgsFile, err := os.CreateTemp("", "")
	if err != nil {
		return err
	}
	defer os.Remove(imageArgsFile.Name())

	for key, value := range imageArgs {
		if _, err := imageArgsFile.WriteString(fmt.Sprintf("%s=%s\n", key, value)); err != nil {
			return err
		}
	}

	args := []string{
		"--output",
		destPath,
		"--type",
		"IMAGE_ARGS",
		imageArgsFile.Name(),
	}

	return z.RunZbiCommand(ctx, args)
}

// Create new ZBI with the system image merkle provided:
// * extract zbi from tempdir
// * extract bootfs from the zbi
// * overwrite bootfs with new system image merkle
// * create zbi manifest to generate new bootfs and zbi
// * generate new bootfs
// * generate new zbi under tempDir
func (z *ZBITool) UpdateZBIWithNewSystemImageMerkle(ctx context.Context,
	systemImageMerkle string,
	pkgDir string,
	bootfsCompression string) error {

	// Create zbitemp directory to store the overwritten zbi
	tempDirForNewZbi, err := os.MkdirTemp("", "")
	if err != nil {
		return fmt.Errorf("failed to create temp directory for zbi: %q", err)
	}
	defer os.RemoveAll(tempDirForNewZbi)

	// Extract zbi from the source update package
	pathToSrcZbi := filepath.Join(pkgDir, "zbi")
	pathToZbiDir := filepath.Join(tempDirForNewZbi, "zbi")
	args := []string{
		"--extract-items",
		"--output-dir",
		pathToZbiDir,
		pathToSrcZbi,
	}
	if err := z.RunZbiCommand(ctx, args); err != nil {
		return fmt.Errorf("failed to extract zbi from %s pakcage: %q", pkgDir, err)
	}

	// Extract bootfs from the zbi extractted from step above
	zbiFiles, err := ioutil.ReadDir(pathToZbiDir)
	if err != nil {
		return fmt.Errorf("fialed to read zbi directory from %s: %q", pathToZbiDir, err)
	}

	pathToZbiBootfs := ""
	for _, file := range zbiFiles {
		if strings.HasSuffix(file.Name(), ".bootfs.zbi") {
			pathToZbiBootfs = filepath.Join(pathToZbiDir, file.Name())
			break
		}
	}
	if pathToZbiBootfs == "" {
		return fmt.Errorf("failed to find bootfs image in zbi from %s", pathToZbiBootfs)
	}

	pathToBootfs := filepath.Join(tempDirForNewZbi, "bootfs")
	args = []string{
		"--extract",
		"--output-dir",
		pathToBootfs,
		pathToZbiBootfs,
	}
	if err := z.RunZbiCommand(ctx, args); err != nil {
		return fmt.Errorf("failed to extract bootfs from %s: %q", pathToZbiBootfs, err)
	}

	// Overwrite system image merkle
	devMgrPath := filepath.Join(pathToBootfs, "config", "additional_boot_args")
	content, err := os.ReadFile(devMgrPath)
	if err != nil {
		return err
	}

	logger.Infof(ctx, "updating the additional boot args config with new system_image_merkle %q", systemImageMerkle)
	lines := strings.Split(string(content), "\n")
	for i, line := range lines {
		if strings.Contains(line, "zircon.system.pkgfs.cmd") {
			lines[i] = basePackagePrefix + systemImageMerkle
		}
	}

	output := strings.Join(lines, "\n")
	if err := os.WriteFile(devMgrPath, []byte(output), 0644); err != nil {
		return fmt.Errorf("failed to update additional boot args: %q", err)
	}

	// Create new zbi manifest file to generate new zbi
	zbiManifest := filepath.Join(tempDirForNewZbi, "manifest")
	zbiManifestFile, err := os.OpenFile(zbiManifest, os.O_APPEND|os.O_WRONLY|os.O_CREATE, 0644)
	if err != nil {
		return fmt.Errorf("failed to create zbi manifest file %q", err)
	}
	defer zbiManifestFile.Close()

	err = filepath.Walk(pathToBootfs, func(path string, info fs.FileInfo, err error) error {
		if err != nil {
			return fmt.Errorf("failed to access a path %q: %v", path, err)
		}

		if !info.IsDir() {
			key, err := filepath.Rel(pathToBootfs, path)
			if err != nil {
				return fmt.Errorf("failed to get realtive paths for %s: %q", pathToBootfs, err)
			}
			if _, err := fmt.Fprintf(zbiManifestFile, "%s=%s\n", key, path); err != nil {
				return fmt.Errorf("failed to generate zbi manifest entries %q", err)
			}
		}

		return nil
	})

	if err != nil {
		return fmt.Errorf("failed to create zbi manifest file: %q", err)
	}

	// Create new bootfs from the zbi manifest
	args = []string{
		"--output",
		pathToSrcZbi,
		"--compressed=" + bootfsCompression,
	}
	for _, file := range zbiFiles {
		if !strings.HasSuffix(file.Name(), ".bootfs.zbi") {
			args = append(args, "--type=container", filepath.Join(pathToZbiDir, file.Name()))
		} else {
			args = append(args, "--files", zbiManifest)
		}
	}

	if err := z.RunZbiCommand(ctx, args); err != nil {
		return fmt.Errorf("failed to extract zbi %q", err)
	}

	return nil
}

func (z *ZBITool) RunZbiCommand(ctx context.Context, args []string) error {
	path, err := exec.LookPath(z.zbiToolPath)
	if err != nil {
		return err
	}

	logger.Infof(ctx, "running: %s %q", path, args)
	cmd := exec.CommandContext(ctx, path, args...)
	if z.stdout != nil {
		cmd.Stdout = z.stdout
	} else {
		cmd.Stdout = os.Stdout
	}
	cmd.Stderr = os.Stderr

	return cmd.Run()
}
