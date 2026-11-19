// Bulk API 2.0 through go-salesforce: InsertBulk / UpdateBulk / UpsertBulk /
// DeleteBulk (and a *File variant), GetJobResults, QueryBulkExport and
// QueryBulkIterator.
package e2e

import (
	"encoding/csv"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/k-capehart/go-salesforce/v3"
)

// bulkClient is a client for the shared session with a generous poll timeout.
func bulkClient(t *testing.T, opts ...salesforce.Option) *salesforce.Salesforce {
	t.Helper()
	return withToken(t, append([]salesforce.Option{salesforce.WithBulkPollTimeout(2 * time.Minute)}, opts...)...)
}

// jobResults returns a finished job's results, tracking the records it created.
func jobResults(t *testing.T, client *salesforce.Salesforce, jobID string) salesforce.BulkJobResults {
	t.Helper()
	res, err := client.GetJobResults(jobID)
	if err != nil {
		t.Fatal(err)
	}
	if res.State != "JobComplete" {
		t.Fatalf("job %s is %s: %s", jobID, res.State, res.ErrorMessage)
	}
	for _, r := range res.SuccessfulRecords {
		if r["sf__Created"] == "true" {
			track(t, fmt.Sprint(r["sf__Id"]))
		}
	}
	return res
}

// bulkNames runs a query through the REST API and returns the sorted names.
func bulkNames(t *testing.T, soql string) []string {
	t.Helper()
	var recs []struct {
		Name string `salesforce:"Name"`
	}
	if err := sf.Query(soql, &recs); err != nil {
		t.Fatal(err)
	}
	out := make([]string, len(recs))
	for i, r := range recs {
		out[i] = r.Name
	}
	sort.Strings(out)
	return out
}

func insertAccounts(t *testing.T, records ...accountRecord) []string {
	t.Helper()
	res, err := sf.InsertCollection("Account", records, 200)
	if err != nil {
		t.Fatal(err)
	}
	ids := make([]string, len(res.Results))
	for i, r := range res.Results {
		if !r.Success {
			t.Fatalf("creating accounts: %+v", r.Errors)
		}
		ids[i] = r.Id
	}
	track(t, ids...)
	return ids
}

func TestBulkIngest(t *testing.T) {
	tag := newTag()
	client := bulkClient(t)

	t.Run("insert, with a result per row", func(t *testing.T) {
		records := []map[string]any{
			{"Name": tag + " insert 1", "Tier__c": "Gold"},
			{"Name": tag + " insert 2, with a comma", "Tier__c": ""},
			{"Name": "", "Tier__c": "Gold"},
		}
		jobs, err := client.InsertBulk("Account", records, 10000, true)
		if err != nil {
			t.Fatal(err)
		}
		res := jobResults(t, client, jobs[0])
		if len(res.SuccessfulRecords) != 2 || len(res.FailedRecords) != 1 || res.NumberRecordsFailed != 1 {
			t.Fatalf("results %+v", res)
		}
		if e := fmt.Sprint(res.FailedRecords[0]["sf__Error"]); !strings.HasPrefix(e, "REQUIRED_FIELD_MISSING:") {
			t.Errorf("sf__Error %q", e)
		}
		if got := bulkNames(t, "SELECT Name FROM Account WHERE Name LIKE '"+tag+" insert%'"); len(got) != 2 || got[1] != tag+" insert 2, with a comma" {
			t.Errorf("names %v", got)
		}
	})

	t.Run("insert in several jobs, gzipped", func(t *testing.T) {
		gz := bulkClient(t, salesforce.WithCompressionHeaders(true))
		records := make([]accountRecord, 5)
		for i := range records {
			records[i] = accountRecord{Name: fmt.Sprintf("%s batch %d", tag, i)}
		}
		jobs, err := gz.InsertBulk("Account", records, 2, true)
		if err != nil {
			t.Fatal(err)
		}
		if len(jobs) != 3 {
			t.Fatalf("jobs %v", jobs)
		}
		for _, j := range jobs {
			jobResults(t, client, j)
		}
		if got := bulkNames(t, "SELECT Name FROM Account WHERE Name LIKE '"+tag+" batch%'"); len(got) != 5 {
			t.Errorf("names %v", got)
		}
	})

	t.Run("insert from a CSV file", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "accounts.csv")
		data := "Name,NumberOfEmployees\n" + tag + " file 1,3\n\"" + tag + " file 2, quoted\",4\n"
		if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
			t.Fatal(err)
		}
		jobs, err := client.InsertBulkFile("Account", path, 10000, true)
		if err != nil {
			t.Fatal(err)
		}
		if res := jobResults(t, client, jobs[0]); len(res.SuccessfulRecords) != 2 {
			t.Fatalf("results %+v", res)
		}
		if got := bulkNames(t, "SELECT Name FROM Account WHERE Name LIKE '"+tag+" file%'"); len(got) != 2 {
			t.Errorf("names %v", got)
		}
	})

	t.Run("update, upsert by an external id, delete", func(t *testing.T) {
		k1, k2 := tag+"-k1", tag+"-k2"
		ids := insertAccounts(t, accountRecord{Name: tag + " old", Key: k1, Tier: "Gold"})

		jobs, err := client.UpdateBulk("Account", []map[string]any{{"Id": ids[0], "Tier__c": "Silver"}}, 10000, true)
		if err != nil {
			t.Fatal(err)
		}
		if res := jobResults(t, client, jobs[0]); len(res.SuccessfulRecords) != 1 || res.SuccessfulRecords[0]["sf__Created"] != "false" {
			t.Fatalf("update results %+v", res)
		}
		var tiers []accountRecord
		if err := sf.Query("SELECT Tier__c FROM Account WHERE Id = '"+ids[0]+"'", &tiers); err != nil || len(tiers) != 1 || tiers[0].Tier != "Silver" {
			t.Fatalf("after update: %+v %v", tiers, err)
		}

		jobs, err = client.UpsertBulk("Account", "External_Key__c", []accountRecord{
			{Name: tag + " upserted", Key: k1},
			{Name: tag + " upsert new", Key: k2},
		}, 10000, true)
		if err != nil {
			t.Fatal(err)
		}
		res := jobResults(t, client, jobs[0])
		created := map[string]string{}
		for _, r := range res.SuccessfulRecords {
			created[fmt.Sprint(r["External_Key__c"])] = fmt.Sprint(r["sf__Created"])
			if r["External_Key__c"] == k1 && r["sf__Id"] != ids[0] {
				t.Errorf("upsert updated %v, not %s", r["sf__Id"], ids[0])
			}
		}
		if created[k1] != "false" || created[k2] != "true" {
			t.Fatalf("upsert results %+v", res)
		}

		var all []accountRecord
		if err := sf.Query("SELECT Id FROM Account WHERE External_Key__c IN ('"+k1+"', '"+k2+"')", &all); err != nil || len(all) != 2 {
			t.Fatalf("%v %v", all, err)
		}
		jobs, err = client.DeleteBulk("Account", all, 10000, true)
		if err != nil {
			t.Fatal(err)
		}
		if res := jobResults(t, client, jobs[0]); len(res.SuccessfulRecords) != 2 {
			t.Fatalf("delete results %+v", res)
		}
		if got := bulkNames(t, "SELECT Name FROM Account WHERE External_Key__c IN ('"+k1+"', '"+k2+"')"); len(got) != 0 {
			t.Errorf("not deleted: %v", got)
		}
	})

	t.Run("an unknown object", func(t *testing.T) {
		_, err := client.InsertBulk("No_Such_Object__c", []map[string]any{{"Name": "x"}}, 10000, true)
		if err == nil {
			t.Fatal("expected an error")
		}
	})

	t.Run("an unknown column fails the job", func(t *testing.T) {
		_, err := client.InsertBulk("Account", []map[string]any{{"Name": tag + " bad column", "No_Such_Field__c": "x"}}, 10000, true)
		if err == nil || !strings.Contains(err.Error(), "No_Such_Field__c") {
			t.Fatalf("expected the job to fail on the column, got %v", err)
		}
		if got := bulkNames(t, "SELECT Name FROM Account WHERE Name = '"+tag+" bad column'"); len(got) != 0 {
			t.Errorf("inserted %v", got)
		}
	})
}

func TestBulkQuery(t *testing.T) {
	tag := newTag()
	const n = 1205
	records := make([]accountRecord, n)
	for i := range records {
		records[i] = accountRecord{Name: fmt.Sprintf("%s page %05d", tag, i)}
	}
	client := bulkClient(t, salesforce.WithBulkQueryMaxRecords(500))
	jobs, err := client.InsertBulk("Account", records, 10000, true)
	if err != nil {
		t.Fatal(err)
	}
	if res := jobResults(t, client, jobs[0]); len(res.SuccessfulRecords) != n {
		t.Fatalf("inserted %d of %d: %+v", len(res.SuccessfulRecords), n, res.FailedRecords)
	}
	soql := "SELECT Id, Name, Owner.Name FROM Account WHERE Name LIKE '" + tag + " page%'"

	t.Run("QueryBulkExport writes every page to one CSV file", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "export.csv")
		if err := client.QueryBulkExport(soql, path); err != nil {
			t.Fatal(err)
		}
		f, err := os.Open(path)
		if err != nil {
			t.Fatal(err)
		}
		defer f.Close()
		rows, err := csv.NewReader(f).ReadAll()
		if err != nil {
			t.Fatal(err)
		}
		if strings.Join(rows[0], ",") != "Id,Name,Owner.Name" {
			t.Errorf("header %v", rows[0])
		}
		seen := map[string]bool{}
		for _, r := range rows[1:] {
			seen[r[0]] = true
			if r[2] == "" {
				t.Fatalf("no owner name in %v", r)
			}
		}
		if len(rows) != n+1 || len(seen) != n {
			t.Errorf("%d rows, %d ids", len(rows)-1, len(seen))
		}
	})

	t.Run("QueryBulkIterator decodes each page", func(t *testing.T) {
		it, err := client.QueryBulkIterator(soql)
		if err != nil {
			t.Fatal(err)
		}
		pages, total := 0, 0
		for it.Next() {
			var page []accountRecord
			if err := it.Decode(&page); err != nil {
				t.Fatal(err)
			}
			if len(page) > 500 {
				t.Errorf("a page of %d", len(page))
			}
			pages++
			total += len(page)
		}
		if err := it.Error(); err != nil {
			t.Fatal(err)
		}
		if pages != 3 || total != n {
			t.Errorf("%d pages, %d records", pages, total)
		}
	})

	t.Run("a bad query is refused", func(t *testing.T) {
		if _, err := client.QueryBulkIterator("SELECT No_Such_Field__c FROM Account"); err == nil || !strings.Contains(err.Error(), "INVALID_FIELD") {
			t.Fatalf("expected INVALID_FIELD, got %v", err)
		}
	})

	t.Run("the results headers", func(t *testing.T) {
		job, err := rawRequest(sf, http.MethodPost, "/jobs/query", strings.NewReader(`{"operation":"query","query":"`+soql+`"}`), nil)
		if err != nil || job.status != 200 {
			t.Fatalf("%v %+v", err, job)
		}
		id := job.body.(map[string]any)["id"].(string)
		for i := 0; i < 600; i++ {
			info, err := rawRequest(sf, http.MethodGet, "/jobs/query/"+id, nil, nil)
			if err != nil {
				t.Fatal(err)
			}
			if info.body.(map[string]any)["state"] == "JobComplete" {
				break
			}
			time.Sleep(time.Second)
		}
		page, err := rawRequest(sf, http.MethodGet, "/jobs/query/"+id+"/results?maxRecords=1000", nil, nil)
		if err != nil || page.status != 200 {
			t.Fatalf("%v %+v", err, page)
		}
		if page.headers.Get("Sforce-NumberOfRecords") != "1000" || page.headers.Get("Sforce-Locator") == "null" {
			t.Errorf("headers %v", page.headers)
		}
		last, err := rawRequest(sf, http.MethodGet, "/jobs/query/"+id+"/results?maxRecords=1000&locator="+page.headers.Get("Sforce-Locator"), nil, nil)
		if err != nil || last.status != 200 {
			t.Fatalf("%v %+v", err, last)
		}
		if last.headers.Get("Sforce-NumberOfRecords") != "205" || last.headers.Get("Sforce-Locator") != "null" {
			t.Errorf("headers %v", last.headers)
		}
	})
}
