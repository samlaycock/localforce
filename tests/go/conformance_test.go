// Runs the shared conformance spec (tests/conformance) through go-salesforce.
//
// How each operation maps to go-salesforce:
//   - create / update / delete: sf.InsertOne, sf.UpdateOne and sf.DeleteOne,
//     with maps.
//   - upsert: sf.DoRequest (PATCH /sobjects/{type}/{field}/{value}). UpsertOne
//     exists, but its SalesforceResult has no `created`, which the
//     expectations and cleanup need. (sdk_test.go covers UpsertOne.)
//   - retrieve, describe, describe_global, composite, retrieve_many:
//     sf.DoRequest; go-salesforce has no API for them.
//   - query: sf.Query into []map[string]any, which follows nextRecordsUrl.
//     It doesn't return totalSize, so when a case checks `total` the runner
//     also asks /query with sf.DoRequest. queryAll (`all`) is sf.DoRequest
//     with paging, since Query only uses /query.
//   - create_many / update_many / delete_many: sf.InsertCollection,
//     sf.UpdateCollection and sf.DeleteCollection. These always send
//     allOrNone=false and give every record the call's type, so
//     `all_or_none` and records of mixed types (`true`) go through
//     sf.DoRequest on /composite/sobjects instead.
//   - upsert_many: sf.DoRequest (PATCH /composite/sobjects/{type}/{field}),
//     since UpsertCollection's results have no `created` either.
//   - request: net/http with the client's session token.
package e2e

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/k-capehart/go-salesforce/v3"
)

func TestConformance(t *testing.T) {
	files, err := loadSpec()
	if err != nil {
		t.Fatal(err)
	}
	for _, spec := range files {
		t.Run(spec.File, func(t *testing.T) {
			fileCtx := &specCtx{tag: newTag(), user: userID, user2: user2ID, version: apiVersion, saved: map[string]any{}}
			var setupIDs []string
			defer func() { cleanUp(setupIDs) }()
			for i, step := range spec.Setup {
				if err := runStep(step, fileCtx, func(ids []string) { setupIDs = append(setupIDs, ids...) }); err != nil {
					t.Fatalf("setup step %d (%s): %v", i+1, opName(step), err)
				}
			}
			for _, c := range spec.Cases {
				t.Run(c.Name, func(t *testing.T) {
					if reason, ok := c.Skip["go"]; ok {
						t.Skip(reason)
					}
					for _, need := range c.Needs {
						if need == "user2" && user2ID == "" {
							t.Skip("needs a second user (LF_SECOND_USERNAME)")
						}
					}
					err := runCase(c, fileCtx.clone())
					switch {
					case err != nil && c.Todo != "" && isLocalforce:
						t.Skipf("todo, not in localforce yet: %s: %v", c.Todo, err)
					case err != nil:
						t.Fatal(err)
					case c.Todo != "" && isLocalforce:
						t.Logf("todo case passes: remove its todo (%s)", c.Todo)
					}
				})
			}
		})
	}
}

func runCase(c specCase, ctx *specCtx) error {
	var ids []string
	defer func() { cleanUp(ids) }()
	for i, step := range c.Steps {
		if err := runStep(step, ctx, func(created []string) { ids = append(ids, created...) }); err != nil {
			return fmt.Errorf("step %d (%s): %w", i+1, opName(step), err)
		}
	}
	return nil
}

var operations = []string{
	"create", "retrieve", "update", "upsert", "delete", "query", "describe", "describe_global",
	"create_many", "update_many", "upsert_many", "delete_many", "retrieve_many", "composite", "request", "sleep", "settle",
}

func opName(step map[string]any) string {
	for _, op := range operations {
		if _, ok := step[op]; ok {
			return op
		}
	}
	return "?"
}

// outcome is what an operation returned, for the expectations.
type outcome struct {
	value      any
	results    []saveResult
	hasResults bool
	created    []string
	status     int
	headers    http.Header
	total      any
}

// toSfError reads a Salesforce error from a go-salesforce error, whose text
// is the raw error response.
func toSfError(err error) *sfError {
	var se *sfError
	if errors.As(err, &se) {
		return se
	}
	text := err.Error()
	var list []map[string]any
	if json.Unmarshal([]byte(text), &list) == nil && len(list) > 0 {
		return errorFromJSON(list[0])
	}
	var obj map[string]any
	if json.Unmarshal([]byte(text), &obj) == nil {
		if code, ok := obj["error"].(string); ok {
			desc, _ := obj["error_description"].(string)
			return &sfError{code: code, message: desc}
		}
	}
	return &sfError{message: text}
}

func errorFromJSON(m map[string]any) *sfError {
	e := &sfError{}
	e.code, _ = m["errorCode"].(string)
	if e.code == "" {
		e.code, _ = m["statusCode"].(string)
	}
	e.message, _ = m["message"].(string)
	if fs, ok := m["fields"].([]any); ok {
		for _, f := range fs {
			e.fields = append(e.fields, fmt.Sprint(f))
		}
	}
	return e
}

// call is sf.DoRequest with a JSON body, returning the decoded response.
func call(method, uri string, body any) (any, int, error) {
	var data []byte
	if body != nil {
		var err error
		if data, err = json.Marshal(body); err != nil {
			return nil, 0, err
		}
	}
	resp, err := sf.DoRequest(method, uri, data)
	if err != nil {
		return nil, 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, 0, err
	}
	var out any
	if len(raw) > 0 {
		if err := json.Unmarshal(raw, &out); err != nil {
			return nil, resp.StatusCode, err
		}
	}
	return out, resp.StatusCode, nil
}

func fromSDKResults(rs []salesforce.SalesforceResult) []saveResult {
	out := make([]saveResult, len(rs))
	for i, r := range rs {
		out[i] = saveResult{ID: r.Id, Success: r.Success}
		for _, e := range r.Errors {
			out[i].Errors = append(out[i].Errors, sfError{code: e.StatusCode, fields: e.Fields, message: e.Message})
		}
	}
	return out
}

func fromJSONResults(v any) ([]saveResult, error) {
	list, ok := v.([]any)
	if !ok {
		return nil, fmt.Errorf("expected a list of save results, got %s", show(v))
	}
	out := make([]saveResult, len(list))
	for i, item := range list {
		m, _ := item.(map[string]any)
		out[i].ID, _ = m["id"].(string)
		out[i].Success, _ = m["success"].(bool)
		if c, ok := m["created"].(bool); ok {
			out[i].Created = &c
		}
		errs, _ := m["errors"].([]any)
		for _, e := range errs {
			em, _ := e.(map[string]any)
			out[i].Errors = append(out[i].Errors, *errorFromJSON(em))
		}
	}
	return out, nil
}

func resultsValue(rs []saveResult) any {
	list := make([]any, len(rs))
	for i, r := range rs {
		m := map[string]any{"id": r.ID, "success": r.Success}
		if r.Created != nil {
			m["created"] = *r.Created
		}
		list[i] = m
	}
	return list
}

func successIDs(rs []saveResult, onlyCreated bool) []string {
	var ids []string
	for _, r := range rs {
		if r.Success && r.ID != "" && (!onlyCreated || (r.Created != nil && *r.Created)) {
			ids = append(ids, r.ID)
		}
	}
	return ids
}

func str(v any) string {
	if s, ok := v.(string); ok {
		return s
	}
	return fmt.Sprint(v)
}

func recordMap(v any) map[string]any {
	m, _ := v.(map[string]any)
	out := make(map[string]any, len(m))
	for k, x := range m {
		out[k] = x
	}
	return out
}

func recordMaps(v any) []map[string]any {
	list, _ := v.([]any)
	out := make([]map[string]any, len(list))
	for i, r := range list {
		out[i] = recordMap(r)
	}
	return out
}

func chunks[T any](list []T, size int) [][]T {
	var out [][]T
	for len(list) > size {
		out = append(out, list[:size])
		list = list[size:]
	}
	if len(list) > 0 || len(out) == 0 {
		out = append(out, list)
	}
	return out
}

// queryPages runs a query (or queryAll) with sf.DoRequest, following
// nextRecordsUrl; it returns the records and the first page's totalSize.
func queryPages(resource, soql string) ([]any, any, error) {
	body, _, err := call(http.MethodGet, "/"+resource+"/?q="+url.QueryEscape(soql), nil)
	if err != nil {
		return nil, nil, err
	}
	first, _ := body.(map[string]any)
	total := first["totalSize"]
	var records []any
	page := first
	for {
		recs, _ := page["records"].([]any)
		records = append(records, recs...)
		next, _ := page["nextRecordsUrl"].(string)
		if done, _ := page["done"].(bool); done || next == "" {
			break
		}
		body, _, err := call(http.MethodGet, strings.TrimPrefix(next, "/services/data/v"+apiVersion), nil)
		if err != nil {
			return nil, nil, err
		}
		page, _ = body.(map[string]any)
	}
	return records, total, nil
}

func operate(step map[string]any) (*outcome, error) {
	allOrNone, _ := step["all_or_none"].(bool)
	switch opName(step) {
	case "create":
		r, err := sf.InsertOne(str(step["create"]), recordMap(step["record"]))
		if err != nil {
			return nil, err
		}
		res := fromSDKResults([]salesforce.SalesforceResult{r})
		if !r.Success && len(res[0].Errors) > 0 {
			return nil, &res[0].Errors[0]
		}
		return &outcome{value: normalise(map[string]any{"id": r.Id, "success": r.Success}), created: []string{r.Id}}, nil

	case "retrieve":
		uri := "/sobjects/" + str(step["retrieve"]) + "/" + str(step["id"])
		if fields, ok := step["fields"].([]any); ok {
			names := make([]string, len(fields))
			for i, f := range fields {
				names[i] = str(f)
			}
			uri += "?fields=" + url.QueryEscape(strings.Join(names, ","))
		}
		body, _, err := call(http.MethodGet, uri, nil)
		if err != nil {
			return nil, err
		}
		return &outcome{value: body}, nil

	case "update":
		rec := recordMap(step["record"])
		rec["Id"] = str(step["id"])
		if err := sf.UpdateOne(str(step["update"]), rec); err != nil {
			return nil, err
		}
		return &outcome{value: map[string]any{"id": str(step["id"]), "success": true}}, nil

	case "upsert":
		uri := fmt.Sprintf("/sobjects/%s/%s/%s", str(step["upsert"]), str(step["field"]), url.PathEscape(str(step["value"])))
		body, _, err := call(http.MethodPatch, uri, recordMap(step["record"]))
		if err != nil {
			return nil, err
		}
		out := &outcome{value: body}
		if m, ok := body.(map[string]any); ok && m["created"] == true {
			out.created = []string{str(m["id"])}
		}
		return out, nil

	case "delete":
		if err := sf.DeleteOne(str(step["delete"]), map[string]any{"Id": str(step["id"])}); err != nil {
			return nil, err
		}
		return &outcome{value: map[string]any{"id": str(step["id"]), "success": true}}, nil

	case "query":
		soql := str(step["query"])
		if all, _ := step["all"].(bool); all {
			records, total, err := queryPages("queryAll", soql)
			if err != nil {
				return nil, err
			}
			return &outcome{value: map[string]any{"records": orEmpty(records)}, total: total}, nil
		}
		var records []map[string]any
		if err := sf.Query(soql, &records); err != nil {
			return nil, err
		}
		out := &outcome{value: map[string]any{"records": orEmpty(normalise(records))}}
		if _, ok := step["total"]; ok {
			body, _, err := call(http.MethodGet, "/query/?q="+url.QueryEscape(soql), nil)
			if err != nil {
				return nil, err
			}
			out.total = body.(map[string]any)["totalSize"]
		}
		return out, nil

	case "describe":
		body, _, err := call(http.MethodGet, "/sobjects/"+str(step["describe"])+"/describe", nil)
		if err != nil {
			return nil, err
		}
		return &outcome{value: body}, nil

	case "describe_global":
		body, _, err := call(http.MethodGet, "/sobjects", nil)
		if err != nil {
			return nil, err
		}
		return &outcome{value: body}, nil

	case "create_many":
		var all []saveResult
		for _, chunk := range chunks(recordMaps(step["records"]), 200) {
			var rs []saveResult
			if typ, ok := step["create_many"].(string); ok && !allOrNone {
				res, err := sf.InsertCollection(typ, chunk, 200)
				if err != nil {
					return nil, err
				}
				rs = fromSDKResults(res.Results)
			} else {
				if typ, ok := step["create_many"].(string); ok {
					for _, r := range chunk {
						r["attributes"] = map[string]any{"type": typ}
					}
				}
				body, _, err := call(http.MethodPost, "/composite/sobjects", map[string]any{"allOrNone": allOrNone, "records": chunk})
				if err != nil {
					return nil, err
				}
				if rs, err = fromJSONResults(body); err != nil {
					return nil, err
				}
			}
			all = append(all, rs...)
		}
		return &outcome{value: resultsValue(all), results: all, hasResults: true, created: successIDs(all, false)}, nil

	case "update_many":
		records := recordMaps(step["records"])
		typ, typed := step["update_many"].(string)
		withIDs := true
		for _, r := range records {
			if id, _ := r["Id"].(string); id == "" {
				withIDs = false
			}
		}
		var rs []saveResult
		if typed && !allOrNone && withIDs {
			res, err := sf.UpdateCollection(typ, records, 200)
			if err != nil {
				return nil, err
			}
			rs = fromSDKResults(res.Results)
		} else {
			if typed {
				for _, r := range records {
					r["attributes"] = map[string]any{"type": typ}
				}
			}
			body, _, err := call(http.MethodPatch, "/composite/sobjects", map[string]any{"allOrNone": allOrNone, "records": records})
			if err != nil {
				return nil, err
			}
			if rs, err = fromJSONResults(body); err != nil {
				return nil, err
			}
		}
		return &outcome{value: resultsValue(rs), results: rs, hasResults: true}, nil

	case "upsert_many":
		typ := str(step["upsert_many"])
		records := recordMaps(step["records"])
		for _, r := range records {
			if _, ok := r["attributes"]; !ok {
				r["attributes"] = map[string]any{"type": typ}
			}
		}
		body, _, err := call(http.MethodPatch, "/composite/sobjects/"+typ+"/"+str(step["field"]), map[string]any{"allOrNone": allOrNone, "records": records})
		if err != nil {
			return nil, err
		}
		rs, err := fromJSONResults(body)
		if err != nil {
			return nil, err
		}
		return &outcome{value: resultsValue(rs), results: rs, hasResults: true, created: successIDs(rs, true)}, nil

	case "delete_many":
		ids, _ := step["ids"].([]any)
		var rs []saveResult
		if !allOrNone {
			records := make([]map[string]any, len(ids))
			for i, id := range ids {
				records[i] = map[string]any{"Id": str(id)}
			}
			res, err := sf.DeleteCollection("sObject", records, 200)
			if err != nil {
				return nil, err
			}
			rs = fromSDKResults(res.Results)
		} else {
			list := make([]string, len(ids))
			for i, id := range ids {
				list[i] = str(id)
			}
			body, _, err := call(http.MethodDelete, "/composite/sobjects?allOrNone=true&ids="+strings.Join(list, ","), nil)
			if err != nil {
				return nil, err
			}
			if rs, err = fromJSONResults(body); err != nil {
				return nil, err
			}
		}
		return &outcome{value: resultsValue(rs), results: rs, hasResults: true}, nil

	case "retrieve_many":
		typ := str(step["retrieve_many"])
		ids, _ := step["ids"].([]any)
		if fields, ok := step["fields"].([]any); ok {
			body, _, err := call(http.MethodPost, "/composite/sobjects/"+typ, map[string]any{"ids": ids, "fields": fields})
			if err != nil {
				return nil, err
			}
			return &outcome{value: body}, nil
		}
		// Without fields, each record is retrieved on its own (like jsforce).
		list := make([]any, len(ids))
		for i, id := range ids {
			body, _, err := call(http.MethodGet, "/sobjects/"+typ+"/"+str(id), nil)
			if err != nil {
				return nil, err
			}
			list[i] = body
		}
		return &outcome{value: list}, nil

	case "composite":
		body, _, err := call(http.MethodPost, "/composite", step["composite"])
		if err != nil {
			return nil, err
		}
		responses, _ := body.(map[string]any)["compositeResponse"].([]any)
		out := &outcome{value: responses}
		for _, r := range responses {
			m, _ := r.(map[string]any)
			b, _ := m["body"].(map[string]any)
			if m["httpStatusCode"] == 201.0 && b["id"] != nil {
				out.created = append(out.created, str(b["id"]))
			}
		}
		return out, nil

	case "request":
		method := str(step["request"])
		headers := map[string]string{}
		if h, ok := step["headers"].(map[string]any); ok {
			for k, v := range h {
				headers[k] = str(v)
			}
		}
		if auth, ok := step["auth"].(bool); ok && !auth {
			headers["Authorization"] = ""
		}
		var body io.Reader
		if rawBody, ok := step["raw_body"]; ok {
			body = strings.NewReader(str(rawBody))
		} else if b, ok := step["body"]; ok {
			data, err := json.Marshal(b)
			if err != nil {
				return nil, err
			}
			body = bytes.NewReader(data)
		}
		res, err := rawRequest(sf, method, str(step["path"]), body, headers)
		if err != nil {
			return nil, err
		}
		out := &outcome{value: res.body, status: res.status, headers: res.headers}
		if m, ok := res.body.(map[string]any); ok && method == "POST" && res.status == 201 && m["id"] != nil {
			out.created = []string{str(m["id"])}
		}
		return out, nil

	case "sleep":
		ms, _ := step["sleep"].(float64)
		time.Sleep(time.Duration(ms) * time.Millisecond)
		return &outcome{}, nil

	case "settle":
		// Only a real org needs time to settle (e.g. to index records for search).
		if !isLocalforce {
			ms, _ := step["settle"].(float64)
			time.Sleep(time.Duration(ms) * time.Millisecond)
		}
		return &outcome{}, nil
	}
	return nil, fmt.Errorf("no operation in step %s", show(step))
}

func orEmpty(v any) any {
	if v == nil {
		return []any{}
	}
	if list, ok := v.([]any); ok && list == nil {
		return []any{}
	}
	return v
}

// runStep runs one step: operate, track what it created, save values, and
// check the expectations.
func runStep(fullStep map[string]any, ctx *specCtx, track func([]string)) error {
	// A template's {i} is filled in per record, below.
	rawStep := make(map[string]any, len(fullStep))
	for k, v := range fullStep {
		if k != "template" {
			rawStep[k] = v
		}
	}
	later := map[string]bool{}
	if save, ok := rawStep["save"].(map[string]any); ok {
		for k := range save {
			later[k] = true
		}
	}
	switch as := rawStep["as"].(type) {
	case string:
		later[as] = true
	case []any:
		for _, n := range as {
			later[str(n)] = true
		}
	}
	pre := *ctx
	pre.later = later
	sub, err := substitute(rawStep, &pre)
	if err != nil {
		return err
	}
	step := sub.(map[string]any)
	if template, ok := fullStep["template"]; ok {
		count, _ := step["count"].(float64)
		records := make([]any, int(count))
		for i := range records {
			c := ctx.clone()
			c.saved["i"] = fmt.Sprintf("%04d", i)
			if records[i], err = substitute(template, c); err != nil {
				return err
			}
		}
		step["records"] = records
	}

	out, err := operate(step)
	if err != nil {
		var m *mismatch
		if errors.As(err, &m) || step["error"] == nil {
			return err
		}
		return matchError(step["error"], toSfError(err), "error")
	}
	track(out.created)
	_, isRequest := step["request"]
	if step["error"] != nil && !isRequest {
		return fmt.Errorf("expected %s, but the call succeeded", show(step["error"]))
	}

	if as, ok := step["as"]; ok {
		var names []string
		switch as := as.(type) {
		case string:
			names = []string{as}
		case []any:
			for _, n := range as {
				names = append(names, str(n))
			}
		}
		var ids []any
		if out.hasResults {
			for _, r := range out.results {
				ids = append(ids, r.ID)
			}
		} else if m, ok := out.value.(map[string]any); ok {
			ids = []any{m["id"]}
		}
		for i, n := range names {
			if i < len(ids) {
				ctx.saved[n] = ids[i]
			}
		}
	}
	_, isQuery := step["query"]
	subject := out.value
	if isQuery {
		subject = dig(out.value, "records.0")
	}
	if save, ok := step["save"].(map[string]any); ok {
		for name, path := range save {
			v := dig(subject, str(path))
			if v == absent {
				v = nil
			}
			ctx.saved[name] = v
		}
	}
	// Expectations can refer to values saved by this step.
	sub, err = substitute(rawStep, ctx)
	if err != nil {
		return err
	}
	expect := sub.(map[string]any)
	return check(step, expect, out)
}

func check(step, expect map[string]any, out *outcome) error {
	has := func(k string) bool { _, ok := expect[k]; return ok }
	op := opName(step)

	if op == "request" {
		if has("status") {
			if err := match(expect["status"], float64(out.status), "status", false); err != nil {
				return err
			}
		}
		if expect["error"] != nil {
			first := &sfError{}
			if list, ok := out.value.([]any); ok && len(list) > 0 {
				m, _ := list[0].(map[string]any)
				first = errorFromJSON(m)
			}
			if err := matchError(expect["error"], first, "error"); err != nil {
				return err
			}
		}
		if has("response") {
			if err := match(expect["response"], out.value, "body", false); err != nil {
				return err
			}
		}
		if hs, ok := expect["response_headers"].(map[string]any); ok {
			for name, re := range hs {
				var v any = absent
				if vals := out.headers.Values(name); len(vals) > 0 {
					v = vals[0]
				}
				var err error
				if re == "$absent" {
					err = match("$absent", v, "header "+name, false)
				} else {
					err = match(map[string]any{"$regex": re}, v, "header "+name, false)
				}
				if err != nil {
					return err
				}
			}
		}
		return nil
	}

	if op == "retrieve" && has("record") {
		if err := match(expect["record"], out.value, "record", false); err != nil {
			return err
		}
	}
	if has("created") {
		m, _ := out.value.(map[string]any)
		var created any = absent
		if c, ok := m["created"]; ok {
			created = c
		}
		if err := match(expect["created"], created, "created", false); err != nil {
			return err
		}
	}
	if has("results") {
		want, _ := expect["results"].([]any)
		if err := matchResults(want, out.results); err != nil {
			return err
		}
	}
	if op == "query" {
		recs, _ := dig(out.value, "records").([]any)
		if recs == nil {
			recs = []any{}
		}
		unordered, _ := expect["unordered"].(bool)
		if has("total") {
			if err := match(expect["total"], out.total, "totalSize", false); err != nil {
				return err
			}
		}
		if has("records") {
			var want any = expect["records"]
			if unordered {
				want = map[string]any{"$unordered": want}
			}
			if err := match(want, any(recs), "records", true); err != nil {
				return err
			}
		}
		if path, ok := expect["pluck"].(string); ok {
			values := make([]any, len(recs))
			for i, r := range recs {
				v := dig(r, path)
				if v == absent {
					v = nil
				}
				values[i] = v
			}
			where := "values of " + path
			if has("values") {
				var want any = expect["values"]
				if unordered {
					want = map[string]any{"$unordered": want}
				}
				if err := match(want, any(values), where, false); err != nil {
					return err
				}
			}
			if has("contains") {
				if err := match(map[string]any{"$contains": expect["contains"]}, any(values), where, false); err != nil {
					return err
				}
			}
			excludes, _ := expect["excludes"].([]any)
			for _, x := range excludes {
				for _, v := range values {
					if reflect.DeepEqual(x, v) {
						return fail(where, "include %s", show(x))
					}
				}
			}
		}
	}
	if op == "retrieve_many" && has("records") {
		if err := match(expect["records"], out.value, "records", false); err != nil {
			return err
		}
	}
	if op == "describe" || op == "describe_global" {
		d, _ := out.value.(map[string]any)
		if has("object") {
			if err := match(expect["object"], any(d), "describe", false); err != nil {
				return err
			}
		}
		lookups := []struct{ expectKey, listKey, nameKey, label string }{
			{"fields", "fields", "name", "field"},
			{"child_relationships", "childRelationships", "relationshipName", "child relationship"},
			{"sobjects", "sobjects", "name", "sobject"},
		}
		for _, l := range lookups {
			wants, _ := expect[l.expectKey].(map[string]any)
			list, _ := d[l.listKey].([]any)
			for name, want := range wants {
				var found any = absent
				for _, item := range list {
					if m, ok := item.(map[string]any); ok && m[l.nameKey] == name {
						found = m
						break
					}
				}
				if err := match(want, found, l.label+" "+name, false); err != nil {
					return err
				}
			}
		}
	}
	if op == "composite" && has("responses") {
		if err := match(expect["responses"], out.value, "compositeResponse", false); err != nil {
			return err
		}
	}
	return nil
}

// cleanUp deletes records newest first, ignoring failures (some are already
// gone).
func cleanUp(ids []string) {
	list := make([]string, len(ids))
	for i, id := range ids {
		list[len(ids)-1-i] = id
	}
	for _, chunk := range chunks(list, 200) {
		if len(chunk) > 0 {
			_, _, _ = call(http.MethodDelete, "/composite/sobjects?allOrNone=false&ids="+strings.Join(chunk, ","), nil)
		}
	}
}
