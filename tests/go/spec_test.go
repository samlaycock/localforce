// The SDK-independent half of the conformance runner: loading the spec,
// placeholders, and matching expected values. tests/node/lib/spec.mjs and
// tests/python implement the same rules; see tests/conformance/README.md.
package e2e

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
)

// specFile is one file of cases.
type specFile struct {
	File  string           `json:"-"`
	About string           `json:"about"`
	Setup []map[string]any `json:"setup"`
	Cases []specCase       `json:"cases"`
}

type specCase struct {
	Name  string            `json:"name"`
	Skip  map[string]string `json:"skip"`
	Todo  string            `json:"todo"`
	Needs []string          `json:"needs"`
	Steps []map[string]any  `json:"steps"`
}

func loadSpec() ([]specFile, error) {
	dir := filepath.Join(repoRoot, "tests", "conformance", "cases")
	names, err := filepath.Glob(filepath.Join(dir, "*.json"))
	if err != nil {
		return nil, err
	}
	sort.Strings(names)
	var files []specFile
	for _, name := range names {
		data, err := os.ReadFile(name)
		if err != nil {
			return nil, err
		}
		var f specFile
		if err := json.Unmarshal(data, &f); err != nil {
			return nil, fmt.Errorf("%s: %w", name, err)
		}
		f.File = strings.TrimSuffix(filepath.Base(name), ".json")
		files = append(files, f)
	}
	return files, nil
}

// newTag is unique to one run of a file: "lf" and 12 lowercase letters and
// digits, e.g. lfx3k2a99f1c2e (cases rely on the length).
func newTag() string {
	b := make([]byte, 3)
	_, _ = rand.Read(b)
	ms := strconv.FormatInt(time.Now().UnixMilli(), 36)
	if len(ms) > 6 {
		ms = ms[len(ms)-6:]
	}
	return "lf" + strings.Repeat("0", 6-len(ms)) + ms + hex.EncodeToString(b)
}

// specCtx is what placeholders are filled in from.
type specCtx struct {
	tag, user, user2, version string
	saved                     map[string]any
	// later are names the current step saves: they're left as placeholders
	// until its expectations are checked.
	later map[string]bool
}

func (c *specCtx) clone() *specCtx {
	saved := make(map[string]any, len(c.saved))
	for k, v := range c.saved {
		saved[k] = v
	}
	cp := *c
	cp.saved = saved
	return &cp
}

var (
	placeholder = regexp.MustCompile(`\{([a-zA-Z_][a-zA-Z0-9_]*)(?::([-+]?\d+))?\}`)
	wholeName   = regexp.MustCompile(`^\{([a-zA-Z_][a-zA-Z0-9_]*)\}$`)
)

func utcDate(days int) string {
	return time.Now().UTC().AddDate(0, 0, days).Format("2006-01-02")
}

// substitute replaces placeholders in every string in value.
func substitute(value any, ctx *specCtx) (any, error) {
	switch v := value.(type) {
	case string:
		if m := wholeName.FindStringSubmatch(v); m != nil {
			if saved, ok := ctx.saved[m[1]]; ok {
				return saved, nil
			}
		}
		var firstErr error
		out := placeholder.ReplaceAllStringFunc(v, func(m string) string {
			parts := placeholder.FindStringSubmatch(m)
			name, arg := parts[1], parts[2]
			days := 0
			if arg != "" {
				days, _ = strconv.Atoi(arg)
			}
			switch name {
			case "tag":
				return ctx.tag
			case "date":
				return utcDate(days)
			case "year":
				return utcDate(days)[:4]
			case "now":
				return time.Now().UTC().Format("2006-01-02T15:04:05.000Z")
			case "user":
				return ctx.user
			case "version":
				return ctx.version
			case "user2":
				if ctx.user2 == "" && firstErr == nil {
					firstErr = fmt.Errorf(`{user2} needs a second user: add "needs": ["user2"] to the case`)
				}
				return ctx.user2
			}
			if saved, ok := ctx.saved[name]; ok {
				s := fmt.Sprint(saved)
				if f, isNum := saved.(float64); isNum {
					s = strconv.FormatFloat(f, 'f', -1, 64)
				}
				if arg == "15" && len(s) >= 15 {
					return s[:15]
				}
				return s
			}
			if ctx.later[name] {
				return m
			}
			if firstErr == nil {
				firstErr = fmt.Errorf("unknown placeholder %s", m)
			}
			return m
		})
		return out, firstErr
	case []any:
		out := make([]any, len(v))
		for i, e := range v {
			s, err := substitute(e, ctx)
			if err != nil {
				return nil, err
			}
			out[i] = s
		}
		return out, nil
	case map[string]any:
		out := make(map[string]any, len(v))
		for k, e := range v {
			s, err := substitute(e, ctx)
			if err != nil {
				return nil, err
			}
			out[k] = s
		}
		return out, nil
	}
	return value, nil
}

// absent stands for a missing key or value (JavaScript's undefined).
type absentT struct{}

var absent = absentT{}

// dig follows a dotted path ("Account.Name", "1.body.id") into a value.
func dig(value any, path string) any {
	for _, k := range strings.Split(path, ".") {
		switch v := value.(type) {
		case map[string]any:
			next, ok := v[k]
			if !ok {
				return absent
			}
			value = next
		case []any:
			i, err := strconv.Atoi(k)
			if err != nil || i < 0 || i >= len(v) {
				return absent
			}
			value = v[i]
		default:
			return absent
		}
	}
	return value
}

// normalise turns any Go value into plain JSON values (map[string]any,
// []any, float64, string, bool, nil).
func normalise(v any) any {
	data, err := json.Marshal(v)
	if err != nil {
		return v
	}
	var out any
	_ = json.Unmarshal(data, &out)
	return out
}

func show(v any) string {
	if _, ok := v.(absentT); ok {
		return "undefined"
	}
	data, err := json.Marshal(v)
	if err != nil {
		return fmt.Sprint(v)
	}
	return string(data)
}

// mismatch is an expectation that didn't hold (as opposed to an error
// calling the API).
type mismatch struct{ msg string }

func (m *mismatch) Error() string { return m.msg }

func fail(where, format string, args ...any) error {
	if where == "" {
		where = "value"
	}
	return &mismatch{where + ": " + fmt.Sprintf(format, args...)}
}

var sfDatetime = regexp.MustCompile(`^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}\+0000$`)

// childRecords unwraps a child relationship ({totalSize, done, records}).
func childRecords(v any) any {
	if m, ok := v.(map[string]any); ok {
		if recs, ok := m["records"].([]any); ok {
			return recs
		}
	}
	return v
}

// match checks actual against expected (see "Matching" in the spec's
// README). exact makes objects need the same keys (apart from attributes),
// as query records do.
func match(expected, actual any, where string, exact bool) error {
	if s, ok := expected.(string); ok && strings.HasPrefix(s, "$") {
		switch s {
		case "$id":
			if a, ok := actual.(string); !ok || !regexp.MustCompile(`^[a-zA-Z0-9]{18}$`).MatchString(a) {
				return fail(where, "expected an 18-character id, got %s", show(actual))
			}
			return nil
		case "$datetime", "$recent":
			a, ok := actual.(string)
			if !ok || !sfDatetime.MatchString(a) {
				return fail(where, "expected a Salesforce datetime, got %s", show(actual))
			}
			if s == "$recent" {
				t, err := time.Parse("2006-01-02T15:04:05.000-0700", a)
				if err != nil || math.Abs(time.Since(t).Minutes()) > 10 {
					return fail(where, "expected a time within 10 minutes of now, got %s", a)
				}
			}
			return nil
		case "$any":
			if actual == absent {
				return fail(where, "expected a value")
			}
			return nil
		case "$notnull":
			if actual == nil || actual == absent {
				return fail(where, "expected a value other than null, got %s", show(actual))
			}
			return nil
		case "$absent":
			if actual != absent {
				return fail(where, "expected no value, got %s", show(actual))
			}
			return nil
		}
	}
	switch exp := expected.(type) {
	case map[string]any:
		if len(exp) == 1 {
			for k, arg := range exp {
				if strings.HasPrefix(k, "$") {
					return matcher(k, arg, actual, where, exact)
				}
			}
		}
		act, ok := actual.(map[string]any)
		if !ok {
			return fail(where, "expected an object, got %s", show(actual))
		}
		keys := make([]string, 0, len(exp))
		for k := range exp {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			sub := k
			if where != "" {
				sub = where + "." + k
			}
			a, has := act[k]
			if exp[k] == "$absent" {
				if has {
					return fail(sub, "expected no such key, got %s", show(a))
				}
				continue
			}
			if !has {
				have := make([]string, 0, len(act))
				for k := range act {
					have = append(have, k)
				}
				sort.Strings(have)
				return fail(sub, "missing (have %s)", strings.Join(have, ", "))
			}
			// An expected attributes object is a subset, even in exact mode.
			if err := match(exp[k], a, sub, exact && k != "attributes"); err != nil {
				return err
			}
		}
		if exact {
			var extra []string
			for k := range act {
				if _, ok := exp[k]; !ok && k != "attributes" {
					extra = append(extra, k)
				}
			}
			if len(extra) > 0 {
				sort.Strings(extra)
				return fail(where, "unexpected keys %s", strings.Join(extra, ", "))
			}
		}
		return nil
	case []any:
		list, ok := childRecords(actual).([]any)
		if !ok {
			return fail(where, "expected an array, got %s", show(actual))
		}
		if len(list) != len(exp) {
			return fail(where, "expected %d items, got %d: %s", len(exp), len(list), show(list))
		}
		for i, e := range exp {
			if err := match(e, list[i], fmt.Sprintf("%s[%d]", where, i), exact); err != nil {
				return err
			}
		}
		return nil
	case float64:
		a, ok := actual.(float64)
		if !ok || math.Abs(a-exp) > 1e-9 {
			return fail(where, "expected %s, got %s", show(exp), show(actual))
		}
		return nil
	}
	if expected != actual {
		return fail(where, "expected %s, got %s", show(expected), show(actual))
	}
	return nil
}

func compare(a, b any) (int, bool) {
	switch x := a.(type) {
	case float64:
		if y, ok := b.(float64); ok {
			switch {
			case x < y:
				return -1, true
			case x > y:
				return 1, true
			}
			return 0, true
		}
	case string:
		if y, ok := b.(string); ok {
			return strings.Compare(x, y), true
		}
	}
	return 0, false
}

func matcher(name string, arg, actual any, where string, exact bool) error {
	switch name {
	case "$regex":
		re, err := regexp.Compile(fmt.Sprint(arg))
		if err != nil {
			return err
		}
		if a, ok := actual.(string); !ok || !re.MatchString(a) {
			return fail(where, "expected a match for /%s/, got %s", arg, show(actual))
		}
		return nil
	case "$approx":
		a, ok := actual.(float64)
		if !ok || math.Abs(a-arg.(float64)) > 0.01 {
			return fail(where, "expected about %s, got %s", show(arg), show(actual))
		}
		return nil
	case "$gt", "$lt":
		c, ok := compare(actual, arg)
		if !ok || (name == "$gt" && c <= 0) || (name == "$lt" && c >= 0) {
			word := "more"
			if name == "$lt" {
				word = "less"
			}
			return fail(where, "expected %s than %s, got %s", word, show(arg), show(actual))
		}
		return nil
	case "$length":
		list, ok := childRecords(actual).([]any)
		if !ok || float64(len(list)) != arg.(float64) {
			return fail(where, "expected %s items, got %s", show(arg), show(actual))
		}
		return nil
	case "$contains", "$unordered":
		list, ok := childRecords(actual).([]any)
		if !ok {
			return fail(where, "expected an array, got %s", show(actual))
		}
		want, _ := arg.([]any)
		if name == "$unordered" && len(list) != len(want) {
			return fail(where, "expected %d items, got %d: %s", len(want), len(list), show(list))
		}
		used := map[int]bool{}
		for _, e := range want {
			found := -1
			for j, a := range list {
				if !used[j] && match(e, a, "", exact) == nil {
					found = j
					break
				}
			}
			if found < 0 {
				return fail(where, "no item matches %s in %s", show(e), show(list))
			}
			used[found] = true
		}
		return nil
	case "$excludes":
		list, ok := childRecords(actual).([]any)
		if !ok {
			return fail(where, "expected an array, got %s", show(actual))
		}
		want, _ := arg.([]any)
		for _, e := range want {
			for _, a := range list {
				if match(e, a, "", exact) == nil {
					return fail(where, "expected no item matching %s in %s", show(e), show(list))
				}
			}
		}
		return nil
	}
	return fail(where, "unknown matcher %s", name)
}

// sfError is a Salesforce error: the first of an error response's errors,
// or a save result's.
type sfError struct {
	code    string
	fields  []string
	message string
}

func (e *sfError) Error() string { return e.code + ": " + e.message }

// matchError checks an error against a step's `error`: a code, a list of
// codes, or {code, fields, message}.
func matchError(expected any, actual *sfError, where string) error {
	var codes []string
	var fields []any
	hasFields := false
	message := ""
	collect := func(c any) {
		switch c := c.(type) {
		case string:
			codes = append(codes, c)
		case []any:
			for _, x := range c {
				codes = append(codes, fmt.Sprint(x))
			}
		}
	}
	switch e := expected.(type) {
	case map[string]any:
		collect(e["code"])
		if f, ok := e["fields"].([]any); ok {
			fields, hasFields = f, true
		}
		if m, ok := e["message"].(string); ok {
			message = m
		}
	default:
		collect(e)
	}
	found := false
	for _, c := range codes {
		if c == actual.code {
			found = true
		}
	}
	if !found {
		return fail(where, "expected %s, got %s: %s", strings.Join(codes, " or "), actual.code, actual.message)
	}
	if hasFields {
		want := make([]string, len(fields))
		for i, f := range fields {
			want[i] = fmt.Sprint(f)
		}
		got := append([]string{}, actual.fields...)
		sort.Strings(want)
		sort.Strings(got)
		if strings.Join(want, ",") != strings.Join(got, ",") {
			return fail(where, "expected fields %s, got %s", show(fields), show(actual.fields))
		}
	}
	if message != "" && !strings.Contains(actual.message, message) {
		return fail(where, "expected the message to contain %q, got %q", message, actual.message)
	}
	return nil
}

// saveResult is one record's result from a collection call.
type saveResult struct {
	ID      string
	Success bool
	Created *bool
	Errors  []sfError
}

// matchResults checks save results against `results`: "ok", {created} or
// {error, fields, message}.
func matchResults(expected []any, actual []saveResult) error {
	if len(actual) != len(expected) {
		return fail("results", "expected %d results, got %d", len(expected), len(actual))
	}
	for i, e := range expected {
		r := actual[i]
		where := fmt.Sprintf("results[%d]", i)
		m, isMap := e.(map[string]any)
		_, isErr := m["error"]
		if e == "ok" || (isMap && !isErr) {
			if !r.Success {
				return fail(where, "expected success, got %s", errorsText(r.Errors))
			}
			if c, ok := m["created"]; ok && isMap {
				if r.Created == nil || *r.Created != c {
					return fail(where, "expected created %v, got %v", c, r.Created)
				}
			}
			continue
		}
		if r.Success {
			return fail(where, "expected %s, got success", show(m["error"]))
		}
		var first sfError
		if len(r.Errors) > 0 {
			first = r.Errors[0]
		}
		if err := matchError(map[string]any{"code": m["error"], "fields": m["fields"], "message": m["message"]}, &first, where); err != nil {
			return err
		}
	}
	return nil
}

func errorsText(errs []sfError) string {
	var parts []string
	for _, e := range errs {
		parts = append(parts, e.Error())
	}
	return "[" + strings.Join(parts, "; ") + "]"
}
