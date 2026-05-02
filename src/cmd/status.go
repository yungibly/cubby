package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

// files/dirs in the store root to always ignore
var storeIgnore = []string{".git", "README.md", "README", "LICENSE"}

type fileStatus int

const (
	statusModified fileStatus = iota
	statusMissing
)

type statusEntry struct {
	rel    string
	status fileStatus
	isDir  bool
}

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show sync status of all tracked files",
	Long: `Compare every file in the storage directory against its live counterpart
and report what is modified or missing from the live system.
If nothing is reported, everything is in sync.`,
	RunE: runStatus,
}

func init() {
	rootCmd.AddCommand(statusCmd)
}

func isIgnored(name string) bool {
	for _, ignored := range storeIgnore {
		if name == ignored {
			return true
		}
	}
	return false
}

func runStatus(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	debugf("home=%s store=%s", home, store)

	if _, err := os.Stat(store); os.IsNotExist(err) {
		errorf("storage directory not found: %s", store)
		return nil
	}

	var modified, missing []statusEntry

	err := filepath.Walk(store, func(storePath string, info os.FileInfo, err error) error {
		if err != nil {
			return nil
		}

		rel, _ := filepath.Rel(store, storePath)

		// skip store root itself and any ignored entries at the root level
		if rel == "." || isIgnored(rel) {
			if info.IsDir() && rel != "." {
				return filepath.SkipDir
			}
			return nil
		}

		livePath := filepath.Join(home, rel)
		debugf("checking %s", rel)

		liveInfo, liveErr := os.Stat(livePath)

		if info.IsDir() {
			if os.IsNotExist(liveErr) {
				missing = append(missing, statusEntry{rel: rel, status: statusMissing, isDir: true})
				return filepath.SkipDir
			}
			return nil
		}

		if os.IsNotExist(liveErr) {
			missing = append(missing, statusEntry{rel: rel, status: statusMissing})
			return nil
		}

		_ = liveInfo
		storeHash, err1 := hashFile(storePath)
		liveHash, err2 := hashFile(livePath)
		if err1 != nil || err2 != nil {
			debugf("could not hash %s: %v %v", rel, err1, err2)
			return nil
		}

		if string(storeHash) != string(liveHash) {
			modified = append(modified, statusEntry{rel: rel, status: statusModified})
		}

		return nil
	})

	if err != nil {
		errorf("error walking storage directory: %v", err)
		return nil
	}

	if len(modified) == 0 && len(missing) == 0 {
		fmt.Printf("%s  %s\n", green("✓"), dim("everything in sync"))
		return nil
	}

	// ── modified ──────────────────────────────────────────────────────────────
	if len(modified) > 0 {
		fmt.Printf("\n%s\n", yellow("● modified"))
		for _, e := range modified {
			liveInfo, _ := os.Stat(filepath.Join(home, e.rel))
			storeInfo, _ := os.Stat(filepath.Join(store, e.rel))
			direction := ""
			if liveInfo != nil && storeInfo != nil {
				if liveInfo.ModTime().After(storeInfo.ModTime()) {
					direction = dim("  live is newer — run: keep " + e.rel)
				} else {
					direction = dim("  store is newer — run: keep restore " + e.rel)
				}
			}
			fmt.Printf("  %s  %s%s\n", yellow("~"), e.rel, direction)
		}
	}

	// ── missing ───────────────────────────────────────────────────────────────
	if len(missing) > 0 {
		fmt.Printf("\n%s\n", red("● missing from live"))
		for _, e := range missing {
			suffix := ""
			if e.isDir {
				suffix = "/"
			}
			fmt.Printf("  %s  %s%s  %s\n",
				red("-"),
				e.rel, suffix,
				dim("run: keep restore "+e.rel),
			)
		}
	}

	// ── summary ───────────────────────────────────────────────────────────────
	fmt.Printf("\n%s\n", dim(fmt.Sprintf(
		"%d modified  ·  %d missing",
		len(modified), len(missing),
	)))

	return nil
}
