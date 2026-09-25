// modcorpus turns Hana's tests that write module files (inTempDir) into
// scenario folders: the files, and each program of the test as main.hr /
// main.knd beside them. tools/runcheck.sh then runs every main in its folder
// with Hana and Haru and compares.
//
//	go run . -out <dir> ../../../hana/tests
package main

import (
	"flag"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

func main() {
	out := flag.String("out", "", "output directory")
	flag.Parse()
	if *out == "" || flag.NArg() != 1 {
		fmt.Fprintln(os.Stderr, "usage: modcorpus -out <dir> <hana/tests>")
		os.Exit(2)
	}
	fset := token.NewFileSet()
	pkgs, err := parser.ParseDir(fset, flag.Arg(0), nil, 0)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	// Package-level string constants: the tests keep programs in them.
	consts := map[string]string{}
	for _, p := range pkgs {
		for _, f := range p.Files {
			for _, d := range f.Decls {
				g, ok := d.(*ast.GenDecl)
				if !ok || (g.Tok != token.CONST && g.Tok != token.VAR) {
					continue
				}
				for _, spec := range g.Specs {
					vs := spec.(*ast.ValueSpec)
					for i, name := range vs.Names {
						if i < len(vs.Values) {
							if s, ok := str(vs.Values[i], nil); ok {
								consts[name.Name] = s
							}
						}
					}
				}
			}
		}
	}
	n := 0
	for _, p := range pkgs {
		for _, f := range p.Files {
			for _, d := range f.Decls {
				fn, ok := d.(*ast.FuncDecl)
				if !ok || fn.Body == nil || !strings.HasPrefix(fn.Name.Name, "Test") {
					continue
				}
				files := map[string]string{}
				var programs []string
				ast.Inspect(fn.Body, func(node ast.Node) bool {
					call, ok := node.(*ast.CallExpr)
					if ok {
						if id, ok := call.Fun.(*ast.Ident); ok && id.Name == "inTempDir" && len(call.Args) == 2 {
							if lit, ok := call.Args[1].(*ast.CompositeLit); ok {
								for _, e := range lit.Elts {
									kv := e.(*ast.KeyValueExpr)
									k, ok1 := str(kv.Key, consts)
									v, ok2 := str(kv.Value, consts)
									if ok1 && ok2 {
										files[k] = v
									}
								}
							}
							return false
						}
					}
					if e, ok := node.(ast.Expr); ok {
						if s, ok := str(e, consts); ok && (strings.Contains(s, "하자") || strings.Contains(s, "しよう") || strings.Contains(s, "そう")) {
							programs = append(programs, s)
							return false
						}
					}
					return true
				})
				if len(files) == 0 {
					continue
				}
				contents := map[string]bool{}
				for _, v := range files {
					contents[v] = true
				}
				for _, prog := range programs {
					if contents[prog] {
						continue
					}
					n++
					dir := filepath.Join(*out, fmt.Sprintf("%04d", n))
					for name, content := range files {
						path := filepath.Join(dir, filepath.FromSlash(name))
						os.MkdirAll(filepath.Dir(path), 0o755)
						os.WriteFile(path, []byte(content), 0o644)
					}
					main := "main.hr"
					if strings.Contains(prog, "しよう") || strings.Contains(prog, "そう") {
						main = "main.knd"
					}
					os.WriteFile(filepath.Join(dir, main), []byte(prog), 0o644)
					os.WriteFile(filepath.Join(dir, "origin.txt"), []byte(fset.Position(fn.Pos()).String()+" "+fn.Name.Name), 0o644)
				}
			}
		}
	}
	fmt.Printf("%d scenarios\n", n)
}

// str evaluates a string literal, a constant, or a concatenation of those.
func str(e ast.Expr, consts map[string]string) (string, bool) {
	switch x := e.(type) {
	case *ast.BasicLit:
		if x.Kind != token.STRING {
			return "", false
		}
		s, err := strconv.Unquote(x.Value)
		return s, err == nil
	case *ast.Ident:
		s, ok := consts[x.Name]
		return s, ok
	case *ast.BinaryExpr:
		if x.Op != token.ADD {
			return "", false
		}
		a, ok1 := str(x.X, consts)
		b, ok2 := str(x.Y, consts)
		return a + b, ok1 && ok2
	case *ast.ParenExpr:
		return str(x.X, consts)
	}
	return "", false
}
