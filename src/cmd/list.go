package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"github.com/spf13/cobra"
)

var listCmd = &cobra.Command{
	Use:   "list",
	Short: "List all tracked files in the storage directory",
	Long:  `Show every file currently in the storage directory as a tree.`,
	RunE:  runList,
}

func init() {
	rootCmd.AddCommand(listCmd)
}

// treeNode represents a file or directory in the store
type treeNode struct {
	name     string
	isDir    bool
	children []*treeNode
}

func buildTree(store, root string) (*treeNode, error) {
	entries, err := os.ReadDir(root)
	if err != nil {
		return nil, err
	}

	rel, _ := filepath.Rel(store, root)
	name := filepath.Base(root)
	if rel == "." {
		name = "."
	}

	node := &treeNode{name: name, isDir: true}

	// sort: dirs first, then files, alphabetically within each group
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].IsDir() != entries[j].IsDir() {
			return entries[i].IsDir()
		}
		return entries[i].Name() < entries[j].Name()
	})

	for _, e := range entries {
		if isIgnored(e.Name()) {
			continue
		}
		child := &treeNode{name: e.Name(), isDir: e.IsDir()}
		if e.IsDir() {
			built, err := buildTree(store, filepath.Join(root, e.Name()))
			if err == nil {
				child.children = built.children
			}
		}
		node.children = append(node.children, child)
	}

	return node, nil
}

func printTree(node *treeNode, prefix string, isLast bool, isRoot bool, total *int) {
	if isRoot {
		fmt.Printf("%s%s%s\n", colorBold, node.name, colorReset)
	} else {
		connector := "├── "
		if isLast {
			connector = "└── "
		}
		if node.isDir {
			fmt.Printf("%s%s%s%s%s\n", dim(prefix+connector), colorBold, node.name+"/", colorReset, "")
		} else {
			fmt.Printf("%s%s\n", dim(prefix+connector), node.name)
			*total++
		}
	}

	childPrefix := prefix
	if !isRoot {
		if isLast {
			childPrefix += "    "
		} else {
			childPrefix += "│   "
		}
	}

	for i, child := range node.children {
		printTree(child, childPrefix, i == len(node.children)-1, false, total)
	}
}

func runList(cmd *cobra.Command, args []string) error {
	store := resolvedStoreDir()

	if _, err := os.Stat(store); os.IsNotExist(err) {
		errorf("storage directory not found: %s", store)
		return nil
	}

	tree, err := buildTree(store, store)
	if err != nil {
		errorf("could not read storage directory: %v", err)
		return nil
	}

	if len(tree.children) == 0 {
		fmt.Println(dim("storage directory is empty."))
		return nil
	}

	total := 0
	fmt.Println()
	printTree(tree, "", false, true, &total)
	fmt.Printf("\n%s\n", dim(fmt.Sprintf("%d files tracked", total)))
	return nil
}
