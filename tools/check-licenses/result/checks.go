// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"fmt"
	"net/http"
	"sort"
	"strings"
	"time"

	classifierLib "github.com/google/licenseclassifier/v2"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/directory"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project/readme"
)

type Check struct {
	Name      string          `json: "name"`
	Allowlist map[string]bool `json:"allowlist"`
}

func RunChecks() error {
	if err := AllFuchsiaAuthorSourceFilesMustHaveCopyrightHeaders(); err != nil {
		return err
	}
	if err := AllFilesAndFoldersMustBeIncludedInAProject(); err != nil {
		return err
	}
	if err := AllLicenseTextsMustBeRecognized(); err != nil {
		return err
	}
	if err := AllLicensePatternUsagesMustBeApproved(); err != nil {
		return err
	}
	if err := AllComplianceWorksheetLinksAreGood(); err != nil {
		return err
	}
	if err := AllProjectsMustHaveALicense(); err != nil {
		return err
	}
	if err := AllReadmeFuchsiaFilesMustBeFormattedCorrectly(); err != nil {
		return err
	}
	return nil
}

// =============================================================================

func AllFuchsiaAuthorSourceFilesMustHaveCopyrightHeaders() error {
	name := "AllFuchsiaAuthorSourceFilesMustHaveCopyrightHeaders"

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	var b strings.Builder
	b.WriteString("All source files owned by The Fuchsia Authors must contain a copyright header.\n")
	b.WriteString("The following files have missing or incorrectly worded copyright header information:\n\n")

	var fuchsia *project.Project
	for _, p := range project.FilteredProjects {
		if p.Root == "." || p.Root == Config.FuchsiaDir {
			fuchsia = p
			break
		}
	}

	if fuchsia == nil {
		return fmt.Errorf("Couldn't find Fuchsia project to verify this check!!\n")
	}
	count := 0
OUTER:
	for _, f := range fuchsia.SearchableRegularFiles {
		if _, ok := allowlist[f.RelPath()]; ok {
			continue
		}

		fdList, err := f.Data()
		if err != nil {
			return fmt.Errorf("Found a file that hasn't been parsed yet?? %v | %v\n", f.AbsPath(), err)
		}

		for _, fd := range fdList {
			results := fd.SearchResults()
			if results != nil {
				for _, m := range results.Matches {
					if m.Name == "FuchsiaCopyright" {
						continue OUTER
					}
				}
			}

			b.WriteString(fmt.Sprintf("-> %v\n", f.RelPath()))
			count = count + 1
		}
	}
	b.WriteString(fmt.Sprintf("\nPlease add the standard Fuchsia copyright header info to the above %v files.\n", count))
	if count > 0 {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllLicenseTextsMustBeRecognized() error {
	name := "AllLicenseTextsMustBeRecognized"

	var foundUnrecognizedMatch bool
	var b strings.Builder

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	b.WriteString("Found unrecognized license texts - please add the relevant license pattern(s) to //tools/check-licenses/license/patterns/* and have it(them) reviewed by the OSRB team:\n\n")

	for _, p := range project.FilteredProjects {
		for _, l := range p.LicenseFiles {
			data, err := l.Data()
			if err != nil {
				return err
			}

			for _, fd := range data {
				results := fd.SearchResults()
				if results == nil {
					foundUnrecognizedMatch = true
					b.WriteString(fmt.Sprintf("-> %s\n", l.RelPath()))
					continue
				}
			}
		}
	}

	if foundUnrecognizedMatch {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllLicensePatternUsagesMustBeApproved() error {
	name := "AllLicensePatternUsagesMustBeApproved"

	var b strings.Builder

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name != name {
			continue
		}
		for k, v := range c.Allowlist {
			allowlist[k] = v
		}
	}

	for _, p := range project.FilteredProjects {
		for _, l := range p.LicenseFiles {
			if _, ok := allowlist[l.RelPath()]; ok {
				continue
			}
			data, _ := l.Data()
			for _, fd := range data {
				results := fd.SearchResults()
				if results == nil {
					continue
				}
				for _, m := range results.Matches {
					if _, ok := allowlist[l.RelPath()]; ok {
						continue
					}

					switch {
					case m.MatchType == "Copyright":
						continue
					case m.MatchType == "Approved":
						continue
					case strings.HasPrefix(m.MatchType, "_"):
						continue
					}

					if isProjectAllowlisted(p.Root, m) {
						continue
					}
					b.WriteString(fmt.Sprintf("File %v from project %s was not approved to use license pattern %s (%s | %s)\n",
						l.RelPath(), p.Root, m.Name, m.MatchType, m.Variant))

				}
			}
		}
	}

	result := b.String()
	if len(result) > 0 {
		return fmt.Errorf("Encountered license texts that were not approved for usage:\n%v", result)
	}
	return nil
}

func isProjectAllowlisted(relpath string, m *classifierLib.Match) bool {
	for _, al := range Config.AllowLists {
		if al.MatchType != m.MatchType || al.Name != m.Name {
			continue
		}
		for _, e := range al.Entries {
			for _, p := range e.Projects {
				if p == relpath {
					return true
				}
			}
		}
	}
	return false

}

func AllComplianceWorksheetLinksAreGood() error {
	if !Config.CheckURLs {
		fmt.Println("Not checking URLs")
		return nil
	}

	numBadLinks := 0
	checkedURLs := make(map[string]bool, 0)
	for _, p := range project.FilteredProjects {
		for _, l := range p.LicenseFiles {
			data, _ := l.Data()
			for _, fd := range data {
				url := fd.URL()
				if checkedURLs[url] {
					continue
				}
				if strings.Contains(url, "3rd party library") {
					continue
				}
				if url != "" {
					fmt.Printf(" -> %s\n", url)
					resp, err := http.Get(url)
					if err != nil {
						fmt.Printf("%v-%v %v: %v \n", p.Root, l.RelPath(), url, err)
						numBadLinks = numBadLinks + 1
					} else if resp.Status != "200 OK" {
						fmt.Printf("%v-%v %v: %v\n", p.Root, l.RelPath(), url, resp.Status)
						numBadLinks = numBadLinks + 1
					}
					checkedURLs[url] = true
					time.Sleep(200 * time.Millisecond)
				}
			}
		}
	}

	if numBadLinks > 0 {
		return fmt.Errorf("Encountered %d bad license URLs.\n", numBadLinks)
	}
	return nil

}

func AllProjectsMustHaveALicense() error {
	name := "AllProjectsMustHaveALicense"

	var b strings.Builder
	b.WriteString("All projects should include relevant license information, and a README.fuchsia file pointing to the file.\n")
	b.WriteString("The following projects were found without any license information:\n\n")

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	badReadmes := make([]string, 0)
	for _, p := range project.FilteredProjects {
		if _, ok := allowlist[p.Root]; ok {
			continue
		}

		if len(p.LicenseFiles) == 0 {
			badReadmes = append(badReadmes, fmt.Sprintf("-> %v (README.fuchsia file: %v)\n", p.Root, p.ReadmeFile.ReadmePath))
		}
	}
	sort.Strings(badReadmes)

	if len(badReadmes) > 0 {
		for _, s := range badReadmes {
			b.WriteString(s)
		}
	}
	b.WriteString("\nPlease add a LICENSE file to the above projects, and point to them in the associated README.fuchsia file.\n")
	if len(badReadmes) > 0 {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllFilesAndFoldersMustBeIncludedInAProject() error {
	name := "AllFilesAndFoldersMustBeIncludedInAProject"

	var b strings.Builder
	count := 0

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	b.WriteString("All files and folders must have proper license attribution.\n")
	b.WriteString("This means a license file needs to accompany all first and third party projects,\n")
	b.WriteString("and a README.fuchsia file must exist and specify where the license file lives.\n")
	b.WriteString("The following directories are not included in a project:\n\n")
	for _, d := range directory.AllDirectories {
		if d.Project == project.UnknownProject && len(d.Files) > 0 {
			if _, ok := allowlist[d.Path]; !ok {
				b.WriteString(fmt.Sprintf("-> %s\n", d.Path))
				count = count + 1
			}
		}
	}

	b.WriteString("\nPlease add a LICENSE file to the above projects, and point to them in an associated README.fuchsia file.\n")
	b.WriteString("If this is urgent and you cannot complete the above steps, add the above paths to the relevant allowlist\n")
	b.WriteString("in tools/check-licenses/result/_config.json (last resort).\n")

	if count > 0 {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllReadmeFuchsiaFilesMustBeFormattedCorrectly() error {
	var b strings.Builder

	b.WriteString("All README.fuchsia files must be formatted correctly, using known directives.\n")
	b.WriteString("https://fuchsia.dev/fuchsia-src/development/source_code/third-party-metadata\n")
	b.WriteString("The following README files are malformed:\n\n")

	count := 0
	for _, r := range readme.AllReadmes {
		if len(r.MalformedLines) > 0 {
			b.WriteString(fmt.Sprintf(" -> %s [%s]\n",
				r.ReadmePath, r.MalformedLines[0]))
			count = count + 1
		}
	}
	b.WriteString("\nPlease fix the above README files.\n")

	if count == 0 {
		return nil
	}
	return fmt.Errorf(b.String())
}
