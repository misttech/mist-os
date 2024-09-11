// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package build

import (
	"encoding/json"
	"log"
	"os"
	"os/exec"
	"path/filepath"
)

// BuildAPIClient is a convenience interface for accessing the build API module
// files from the build system, using the //build/api/client script.
type BuildAPIClient struct {
	buildDir string
	toolPath string
	// lastBuildOnly is true if this instance should return results
	// that only contain entries related to the last successful build.
	lastBuildOnly bool
}

// NewBuildAPIClient returns a BuildAPIClient associated with a given build
// directory. Note that `gn gen` must be run once to generate the
// $buildDir/build_api_client_path file which will be used to locate
// the script from the checkout directory.
func NewBuildAPIClient(buildDir string, lastBuildOnly bool) (*BuildAPIClient, error) {
	relativePath, err := os.ReadFile(filepath.Join(buildDir, "build_api_client_path"))
	if err != nil {
		return nil, err
	}
	toolPath := filepath.Join(buildDir, string(relativePath))
	if _, err := os.Stat(toolPath); err != nil {
		return nil, err
	}
	c := &BuildAPIClient{buildDir, toolPath, lastBuildOnly}
	return c, nil
}

// GetRaw returns the content of a build API module file as a raw string.
func (c BuildAPIClient) GetRaw(name string) ([]byte, error) {
	args := []string{"--build-dir", c.buildDir, "print"}
	if c.lastBuildOnly {
		args = append(args, "--last-build-only")
	}
	args = append(args, name)
	cmd := exec.Command(c.toolPath, args...)
	cmd.Stderr = os.Stderr
	output, err := cmd.Output()
	if err != nil {
		log.Fatal(err)
	}
	return output, err
}

// GetJSON reads build API module file as JSON.
func (c BuildAPIClient) GetJSON(name string, v interface{}) error {
	content, err := c.GetRaw(name)
	if err != nil {
		return err
	}
	return json.Unmarshal(content, v)
}
