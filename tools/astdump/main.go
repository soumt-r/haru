// astdump writes Hana's syntax trees for a corpus of Hari/Kanade programs, so
// Haru's parser can be checked tree for tree (`haru ast-check <dir>`).
//
//	go run . -out <dir> <path>...
//
// Each path is walked for .hr/.knd files, ```hari/```kanade blocks in
// .md/.mdx files, and program-looking string literals in .go files (Hana's
// tests). Every distinct program becomes <dir>/NNNN.hr|.knd plus NNNN.json
// (the tree in the form of haru-syntax's dump.rs), and index.tsv says where
// each came from. Programs that make Hana's parser panic are skipped.
//
// -mutate N adds N damaged copies of every program (a line removed, doubled
// or swapped, the text cut short, a span of characters removed or repeated):
// odd input reaches the parser's recovery paths, which real programs rarely do.
package main

import (
	"crypto/sha256"
	"flag"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"math/rand"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"strconv"
	"strings"
	"unicode/utf8"

	harilexer "github.com/soumt-r/hana/lexer/hari"
	kanadelexer "github.com/soumt-r/hana/lexer/kanade"
	hariparser "github.com/soumt-r/hana/parser/hari"
	kanadeparser "github.com/soumt-r/hana/parser/kanade"
)

var (
	hariBlock   = regexp.MustCompile("(?s)```hari([^\\n]*)\\n(.*?)```")
	kanadeBlock = regexp.MustCompile("(?s)```kanade([^\\n]*)\\n(.*?)```")
)

type program struct {
	source, origin string
	kanade         bool
}

func main() {
	out := flag.String("out", "", "output directory")
	mutate := flag.Int("mutate", 0, "damaged copies per program")
	flag.Parse()
	if *out == "" || flag.NArg() == 0 {
		fmt.Fprintln(os.Stderr, "usage: astdump -out <dir> <path>...")
		os.Exit(2)
	}
	var progs []program
	for _, root := range flag.Args() {
		filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				return nil
			}
			if d.IsDir() {
				if name := d.Name(); name == "node_modules" || name == ".git" || name == "target" {
					return filepath.SkipDir
				}
				return nil
			}
			progs = append(progs, collect(path)...)
			return nil
		})
	}

	if *mutate > 0 {
		rng := rand.New(rand.NewSource(1))
		n := len(progs)
		for i := 0; i < n; i++ {
			for k := 0; k < *mutate; k++ {
				m := progs[i]
				m.source = damage(rng, m.source)
				m.origin = fmt.Sprintf("%s (mutant %d)", m.origin, k+1)
				progs = append(progs, m)
			}
		}
	}

	os.MkdirAll(*out, 0o755)
	seen := map[[32]byte]bool{}
	var index strings.Builder
	n, skipped := 0, 0
	for _, p := range progs {
		if !utf8.ValidString(p.source) {
			continue
		}
		key := sha256.Sum256([]byte(fmt.Sprint(p.kanade) + p.source))
		if seen[key] {
			continue
		}
		seen[key] = true
		tree, ok := parse(p)
		if !ok {
			skipped++
			continue
		}
		n++
		ext := ".hr"
		if p.kanade {
			ext = ".knd"
		}
		base := filepath.Join(*out, fmt.Sprintf("%04d", n))
		os.WriteFile(base+ext, []byte(p.source), 0o644)
		os.WriteFile(base+".json", []byte(tree), 0o644)
		fmt.Fprintf(&index, "%04d%s\t%s\n", n, ext, p.origin)
	}
	os.WriteFile(filepath.Join(*out, "index.tsv"), []byte(index.String()), 0o644)
	fmt.Printf("%d programs (%d skipped: Hana's parser panicked)\n", n, skipped)
}

func damage(rng *rand.Rand, s string) string {
	lines := strings.Split(s, "\n")
	runes := []rune(s)
	switch rng.Intn(6) {
	case 0: // remove a line
		i := rng.Intn(len(lines))
		lines = append(lines[:i:i], lines[i+1:]...)
	case 1: // double a line
		i := rng.Intn(len(lines))
		lines = append(lines[:i+1:i+1], lines[i:]...)
	case 2: // swap two lines
		i, j := rng.Intn(len(lines)), rng.Intn(len(lines))
		lines[i], lines[j] = lines[j], lines[i]
	case 3: // cut short
		return string(runes[:rng.Intn(len(runes)+1)])
	case 4: // remove a span
		if len(runes) == 0 {
			return s
		}
		i := rng.Intn(len(runes))
		j := i + 1 + rng.Intn(8)
		if j > len(runes) {
			j = len(runes)
		}
		return string(runes[:i]) + string(runes[j:])
	case 5: // repeat a span
		if len(runes) == 0 {
			return s
		}
		i := rng.Intn(len(runes))
		j := i + 1 + rng.Intn(8)
		if j > len(runes) {
			j = len(runes)
		}
		return string(runes[:j]) + string(runes[i:])
	}
	return strings.Join(lines, "\n")
}

func collect(path string) []program {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	text := string(data)
	switch filepath.Ext(path) {
	case ".hr":
		return []program{{text, path, false}}
	case ".knd":
		return []program{{text, path, true}}
	case ".md", ".mdx":
		var ps []program
		for _, m := range hariBlock.FindAllStringSubmatch(text, -1) {
			ps = append(ps, program{m[2], path + " (hari block)", false})
		}
		for _, m := range kanadeBlock.FindAllStringSubmatch(text, -1) {
			ps = append(ps, program{m[2], path + " (kanade block)", true})
		}
		return ps
	case ".go":
		return goStrings(path, data)
	}
	return nil
}

// goStrings finds string literals in a Go file that look like programs.
func goStrings(path string, data []byte) []program {
	fset := token.NewFileSet()
	f, err := parser.ParseFile(fset, path, data, 0)
	if err != nil {
		return nil
	}
	var ps []program
	ast.Inspect(f, func(n ast.Node) bool {
		lit, ok := n.(*ast.BasicLit)
		if !ok || lit.Kind != token.STRING {
			return true
		}
		s, err := strconv.Unquote(lit.Value)
		if err != nil {
			return true
		}
		origin := fmt.Sprintf("%s:%d", path, fset.Position(lit.Pos()).Line)
		switch {
		case strings.Contains(s, "しよう") || strings.Contains(s, "返そう") || strings.Contains(s, "繰り返そう"):
			ps = append(ps, program{s, origin, true})
		case strings.Contains(s, "하자"):
			ps = append(ps, program{s, origin, false})
		}
		return true
	})
	return ps
}

func parse(p program) (tree string, ok bool) {
	defer func() {
		if recover() != nil {
			ok = false
		}
	}()
	var ps *hariparser.Parser
	if p.kanade {
		ps = kanadeparser.New(kanadelexer.New(p.source))
	} else {
		ps = hariparser.New(harilexer.New(p.source))
	}
	prog := ps.ParseProgram()
	var b strings.Builder
	dump(&b, reflect.ValueOf(prog), "")
	// Then one line per reported syntax problem: line:col:length:literal.
	for _, d := range ps.Diagnostics() {
		fmt.Fprintf(&b, "\n%d:%d:%d:%s", d.Line, d.Col, d.Length, d.Literal)
	}
	return b.String(), true
}

// Fields (all interface{}) that are engine caches or bookkeeping, not syntax.
var skip = map[string]bool{"Module": true, "Boxed": true, "Cooked": true, "Parts": true}

// Where a declaration is written (for the parser's diagnostics, which are
// compared on their own).
var position = map[string]bool{"SrcLine": true, "SrcCol": true, "SrcLen": true}

func dump(b *strings.Builder, v reflect.Value, field string) {
	switch v.Kind() {
	case reflect.Interface, reflect.Ptr:
		if v.IsNil() {
			b.WriteString("null")
			return
		}
		dump(b, v.Elem(), field)
	case reflect.Struct:
		t := v.Type()
		b.WriteString(`{"type":`)
		str(b, t.Name())
		for i := 0; i < t.NumField(); i++ {
			f := t.Field(i)
			if !f.IsExported() || (skip[f.Name] && f.Type.Kind() == reflect.Interface) || position[f.Name] {
				continue
			}
			b.WriteString(",")
			str(b, f.Name)
			b.WriteString(":")
			// A property without a getter (nil) differs from an empty getter.
			if f.Name == "Getter" && v.Field(i).IsNil() {
				b.WriteString("null")
				continue
			}
			dump(b, v.Field(i), f.Name)
		}
		b.WriteString("}")
	case reflect.Slice:
		b.WriteString("[")
		for i := 0; i < v.Len(); i++ {
			if i > 0 {
				b.WriteString(",")
			}
			dump(b, v.Index(i), field)
		}
		b.WriteString("]")
	case reflect.String:
		str(b, v.String())
	case reflect.Bool:
		b.WriteString(strconv.FormatBool(v.Bool()))
	case reflect.Float64:
		b.WriteString(strconv.FormatFloat(v.Float(), 'f', -1, 64))
	default:
		panic("astdump: unexpected " + v.Kind().String())
	}
}

func str(b *strings.Builder, s string) {
	b.WriteByte('"')
	for _, c := range s {
		switch {
		case c == '"':
			b.WriteString(`\"`)
		case c == '\\':
			b.WriteString(`\\`)
		case c == '\n':
			b.WriteString(`\n`)
		case c == '\r':
			b.WriteString(`\r`)
		case c == '\t':
			b.WriteString(`\t`)
		case c < 0x20:
			fmt.Fprintf(b, `\u%04x`, c)
		default:
			b.WriteRune(c)
		}
	}
	b.WriteByte('"')
}
