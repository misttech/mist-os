// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package directory

import (
	"encoding/json"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
)

var AllDirectories map[string]*Directory
var RootDirectory *Directory

func init() {
	AllDirectories = make(map[string]*Directory, 0)
}

func Initialize(c *DirectoryConfig) error {
	// Project readme directories should always be skipped.
	// They are processed during the project Initialize() call.
	readmeSkips := &Skip{
		Paths: []string{},
		Notes: []string{"Always skip project.Readmes paths."},
	}
	for _, r := range project.Config.Readmes {
		readmeSkips.Paths = append(readmeSkips.Paths, r.Paths...)
	}
	c.Skips = append(c.Skips, readmeSkips)

	// check-licenses License pattern directories should also be skipped.
	patternSkips := &Skip{
		Paths: []string{},
		Notes: []string{"Always skip license.PatternRoot paths."},
	}
	c.Skips = append(c.Skips, patternSkips)

	// Save the config file to the out directory (if defined).
	if b, err := json.MarshalIndent(c, "", "  "); err != nil {
		return err
	} else {
		plusFile("_config.json", b)
	}

	Config = c
	return nil
}
