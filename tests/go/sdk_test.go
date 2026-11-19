// go-salesforce APIs the shared spec can't express: the login flows, decoding
// into structs, go-soql query structs, batching, the composite helpers,
// session refresh and compression.
package e2e

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/forcedotcom/go-soql"
	"github.com/k-capehart/go-salesforce/v3"
)

// track deletes the records with these ids when the test ends.
func track(t *testing.T, ids ...string) {
	t.Helper()
	t.Cleanup(func() { cleanUp(ids) })
}

// withToken is a client for the shared session, with options.
func withToken(t *testing.T, opts ...salesforce.Option) *salesforce.Salesforce {
	t.Helper()
	client, err := salesforce.Init(salesforce.Creds{Domain: sf.GetInstanceUrl(), AccessToken: sf.GetAccessToken()}, opts...)
	if err != nil {
		t.Fatal(err)
	}
	return client
}

func passwordCreds(username string) salesforce.Creds {
	return salesforce.Creds{
		Domain:         loginURL,
		Username:       username,
		Password:       os.Getenv("SF_PASSWORD"),
		SecurityToken:  envOr("SF_SECURITY_TOKEN", "unused"),
		ConsumerKey:    os.Getenv("SF_CLIENT_ID"),
		ConsumerSecret: os.Getenv("SF_CLIENT_SECRET"),
	}
}

func needsPassword(t *testing.T) {
	t.Helper()
	if !hasPassword() {
		t.Skip("needs SF_USERNAME, SF_PASSWORD, SF_CLIENT_ID and SF_CLIENT_SECRET")
	}
}

// jwtCreds signs JWT bearer assertions for username with a throwaway key.
func jwtCreds(t *testing.T, username string) salesforce.Creds {
	t.Helper()
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	pemKey := pem.EncodeToMemory(&pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(key)})
	return salesforce.Creds{Domain: loginURL, Username: username, ConsumerKey: "localforce-tests", ConsumerRSAPem: string(pemKey)}
}

func TestLogin(t *testing.T) {
	t.Run("username-password", func(t *testing.T) {
		needsPassword(t)
		client, err := salesforce.Init(passwordCreds(os.Getenv("SF_USERNAME")))
		if err != nil {
			t.Fatal(err)
		}
		if client.GetAuthFlow() != salesforce.AuthFlowUsernamePassword {
			t.Errorf("auth flow %v", client.GetAuthFlow())
		}
		if !strings.HasPrefix(client.GetInstanceUrl(), "http") || client.GetAccessToken() == "" {
			t.Errorf("instance %q, token %q", client.GetInstanceUrl(), client.GetAccessToken())
		}
		user, _, err := userIDs(client)
		if err != nil {
			t.Fatal(err)
		}
		if user != userID {
			t.Errorf("logged in as %s, want %s", user, userID)
		}
	})

	t.Run("client credentials", func(t *testing.T) {
		if !isLocalforce {
			t.Skip("needs a connected app with a run-as user")
		}
		client, err := salesforce.Init(salesforce.Creds{Domain: loginURL, ConsumerKey: "any", ConsumerSecret: "any"})
		if err != nil {
			t.Fatal(err)
		}
		if client.GetAuthFlow() != salesforce.AuthFlowClientCredentials {
			t.Errorf("auth flow %v", client.GetAuthFlow())
		}
		var users []map[string]any
		if err := client.Query("SELECT Id FROM User LIMIT 1", &users); err != nil {
			t.Fatal(err)
		}
	})

	t.Run("access token, validated", func(t *testing.T) {
		client := withToken(t, salesforce.WithValidateAuthentication(true))
		if client.GetAuthFlow() != salesforce.AuthFlowAccessToken {
			t.Errorf("auth flow %v", client.GetAuthFlow())
		}
	})

	t.Run("a bad access token fails validation", func(t *testing.T) {
		_, err := salesforce.Init(
			salesforce.Creds{Domain: sf.GetInstanceUrl(), AccessToken: "00Dxx0000000000!not-a-real-session"},
			salesforce.WithValidateAuthentication(true),
		)
		// go-salesforce gets INVALID_SESSION_ID, can't refresh an access
		// token, and says so.
		if err == nil || !strings.Contains(err.Error(), "invalid session") {
			t.Fatalf("expected an invalid session, got %v", err)
		}
	})

	t.Run("JWT bearer", func(t *testing.T) {
		if !isLocalforce {
			t.Skip("needs a connected app with a certificate")
		}
		client, err := salesforce.Init(jwtCreds(t, os.Getenv("SF_USERNAME")))
		if err != nil {
			t.Fatal(err)
		}
		if client.GetAuthFlow() != salesforce.AuthFlowJWT {
			t.Errorf("auth flow %v", client.GetAuthFlow())
		}
		user, _, err := userIDs(client)
		if err != nil {
			t.Fatal(err)
		}
		if user != userID {
			t.Errorf("logged in as %s, want %s", user, userID)
		}
		if _, err := salesforce.Init(jwtCreds(t, "nobody-at-all@example.invalid")); err == nil {
			t.Error("an unknown user logged in with a JWT")
		}
	})

	t.Run("an unknown user can't log in", func(t *testing.T) {
		needsPassword(t)
		_, err := salesforce.Init(passwordCreds("nobody-at-all@example.invalid"))
		if err == nil || !strings.Contains(err.Error(), "failed authentication") {
			t.Fatalf("expected an authentication failure, got %v", err)
		}
	})
}

type testContact struct {
	Id        string           `salesforce:"Id"`
	LastName  string           `salesforce:"LastName"`
	Email     *string          `salesforce:"Email"`
	Account   *testAccountName `salesforce:"Account"`
	CreatedAt time.Time        `salesforce:"CreatedDate"`
}

type testAccountName struct {
	Name string `salesforce:"Name"`
}

type testAccount struct {
	Id            string   `salesforce:"Id"`
	Name          string   `salesforce:"Name"`
	AnnualRevenue *float64 `salesforce:"AnnualRevenue"`
	Contacts      *struct {
		TotalSize int           `salesforce:"totalSize"`
		Records   []testContact `salesforce:"records"`
	} `salesforce:"Contacts"`
}

func TestQueryIntoStructs(t *testing.T) {
	tag := newTag()
	acct, err := sf.InsertOne("Account", map[string]any{"Name": tag + " struct", "AnnualRevenue": 1500.5})
	if err != nil {
		t.Fatal(err)
	}
	track(t, acct.Id)
	for _, name := range []string{"Bee", "Ant"} {
		c, err := sf.InsertOne("Contact", map[string]any{"LastName": tag + " " + name, "AccountId": acct.Id, "Email": strings.ToLower(name) + "@example.com"})
		if err != nil {
			t.Fatal(err)
		}
		track(t, c.Id)
	}

	t.Run("parent relationships and datetimes", func(t *testing.T) {
		var contacts []testContact
		soql := fmt.Sprintf("SELECT Id, LastName, Email, Account.Name, CreatedDate FROM Contact WHERE AccountId = '%s' ORDER BY LastName", acct.Id)
		if err := sf.Query(soql, &contacts); err != nil {
			t.Fatal(err)
		}
		if len(contacts) != 2 || contacts[0].LastName != tag+" Ant" {
			t.Fatalf("contacts %+v", contacts)
		}
		if contacts[0].Account == nil || contacts[0].Account.Name != tag+" struct" {
			t.Errorf("Account %+v", contacts[0].Account)
		}
		if contacts[0].Email == nil || *contacts[0].Email != "ant@example.com" {
			t.Errorf("Email %v", contacts[0].Email)
		}
		if d := time.Since(contacts[0].CreatedAt); d < -10*time.Minute || d > 10*time.Minute {
			t.Errorf("CreatedDate decoded as %v", contacts[0].CreatedAt)
		}
	})

	t.Run("child subqueries and numbers", func(t *testing.T) {
		var accounts []testAccount
		soql := fmt.Sprintf("SELECT Id, Name, AnnualRevenue, (SELECT LastName FROM Contacts ORDER BY LastName) FROM Account WHERE Id = '%s'", acct.Id)
		if err := sf.Query(soql, &accounts); err != nil {
			t.Fatal(err)
		}
		if len(accounts) != 1 {
			t.Fatalf("accounts %+v", accounts)
		}
		a := accounts[0]
		if a.AnnualRevenue == nil || *a.AnnualRevenue != 1500.5 {
			t.Errorf("AnnualRevenue %v", a.AnnualRevenue)
		}
		if a.Contacts == nil || a.Contacts.TotalSize != 2 || len(a.Contacts.Records) != 2 || a.Contacts.Records[1].LastName != tag+" Bee" {
			t.Errorf("Contacts %+v", a.Contacts)
		}
	})

	t.Run("query errors", func(t *testing.T) {
		var out []map[string]any
		err := sf.Query("SELECT Nope FROM Account", &out)
		if err == nil || toSfError(err).code != "INVALID_FIELD" {
			t.Fatalf("expected INVALID_FIELD, got %v", err)
		}
	})
}

type contactQuery struct {
	SelectClause contactColumns  `soql:"selectClause,tableName=Contact"`
	WhereClause  contactCriteria `soql:"whereClause"`
	OrderBy      []soql.Order    `soql:"orderByClause"`
	Limit        *int            `soql:"limitClause"`
}

type contactColumns struct {
	Id       string            `soql:"selectColumn,fieldName=Id"`
	LastName string            `soql:"selectColumn,fieldName=LastName"`
	Account  accountNameColumn `soql:"selectColumn,fieldName=Account"`
}

type accountNameColumn struct {
	Name string `soql:"selectColumn,fieldName=Name"`
}

type contactCriteria struct {
	LastName []string `soql:"likeOperator,fieldName=LastName"`
	Email    *bool    `soql:"nullOperator,fieldName=Email"`
}

func TestQueryStruct(t *testing.T) {
	tag := newTag()
	acct, err := sf.InsertOne("Account", map[string]any{"Name": tag + " soql"})
	if err != nil {
		t.Fatal(err)
	}
	track(t, acct.Id)
	res, err := sf.InsertCollection("Contact", []map[string]any{
		{"LastName": tag + " A", "AccountId": acct.Id, "Email": "a@example.com"},
		{"LastName": tag + " B", "AccountId": acct.Id},
		{"LastName": tag + " C", "AccountId": acct.Id, "Email": "c@example.com"},
	}, 200)
	if err != nil || res.HasSalesforceErrors {
		t.Fatal(err, res)
	}
	for _, r := range res.Results {
		track(t, r.Id)
	}

	hasEmail := false
	limit := 5
	q := contactQuery{
		// go-soql wraps LIKE values in % and escapes their own wildcards:
		// LastName LIKE '%<tag>%' AND Email != null.
		WhereClause: contactCriteria{LastName: []string{tag}, Email: &hasEmail},
		OrderBy:     []soql.Order{{Field: "LastName", IsDesc: true}},
		Limit:       &limit,
	}
	var contacts []testContact
	if err := sf.QueryStruct(q, &contacts); err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, c := range contacts {
		names = append(names, strings.TrimPrefix(c.LastName, tag+" "))
		if c.Account == nil || c.Account.Name != tag+" soql" {
			t.Errorf("Account %+v", c.Account)
		}
	}
	if strings.Join(names, ",") != "C,A" {
		t.Errorf("got %v, want C,A (has an email, newest name first)", names)
	}
}

type projectRecord struct {
	Id        string  `salesforce:"Id,omitempty"`
	Name      string  `salesforce:"Name,omitempty"`
	Code      string  `salesforce:"Code__c,omitempty"`
	Budget    float64 `salesforce:"Budget__c,omitempty"`
	Status    string  `salesforce:"Status__c,omitempty"`
	StartDate string  `salesforce:"Start_Date__c,omitempty"`
}

type accountRecord struct {
	Id   string `salesforce:"Id,omitempty"`
	Name string `salesforce:"Name,omitempty"`
	Key  string `salesforce:"External_Key__c,omitempty"`
	Tier string `salesforce:"Tier__c,omitempty"`
}

func TestSingleRecordsWithStructs(t *testing.T) {
	tag := newTag()
	created, err := sf.InsertOne("Project__c", projectRecord{Name: tag + " struct", Budget: 1250.75, StartDate: "2026-03-01"})
	if err != nil || !created.Success {
		t.Fatal(err, created)
	}
	track(t, created.Id)

	if err := sf.UpdateOne("Project__c", projectRecord{Id: created.Id, Status: "Active"}); err != nil {
		t.Fatal(err)
	}
	var got []projectRecord
	if err := sf.Query("SELECT Id, Name, Budget__c, Status__c, Start_Date__c FROM Project__c WHERE Id = '"+created.Id+"'", &got); err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Status != "Active" || got[0].Budget != 1250.75 || got[0].StartDate != "2026-03-01" || got[0].Name != tag+" struct" {
		t.Fatalf("got %+v", got)
	}

	t.Run("upsert creates, then updates", func(t *testing.T) {
		key := tag + "-UPS"
		first, err := sf.UpsertOne("Account", "External_Key__c", accountRecord{Name: tag + " upsert", Key: key})
		if err != nil || !first.Success || first.Id == "" {
			t.Fatal(err, first)
		}
		track(t, first.Id)
		again, err := sf.UpsertOne("Account", "External_Key__c", accountRecord{Key: key, Tier: "Gold"})
		if err != nil || again.Id != first.Id {
			t.Fatalf("second upsert: %v %+v, want id %s", err, again, first.Id)
		}
		var accts []accountRecord
		if err := sf.Query("SELECT Name, Tier__c FROM Account WHERE Id = '"+first.Id+"'", &accts); err != nil {
			t.Fatal(err)
		}
		if len(accts) != 1 || accts[0].Tier != "Gold" || accts[0].Name != tag+" upsert" {
			t.Errorf("got %+v", accts)
		}
	})

	t.Run("errors carry Salesforce's code and fields", func(t *testing.T) {
		_, err := sf.InsertOne("Account", accountRecord{Tier: "Gold"})
		if e := toSfError(err); err == nil || e.code != "REQUIRED_FIELD_MISSING" || strings.Join(e.fields, ",") != "Name" {
			t.Fatalf("expected REQUIRED_FIELD_MISSING on Name, got %v", err)
		}
	})

	t.Run("delete", func(t *testing.T) {
		if err := sf.DeleteOne("Project__c", projectRecord{Id: created.Id}); err != nil {
			t.Fatal(err)
		}
		err := sf.DeleteOne("Project__c", projectRecord{Id: created.Id})
		if e := toSfError(err); err == nil || e.code != "ENTITY_IS_DELETED" {
			t.Fatalf("deleting twice: expected ENTITY_IS_DELETED, got %v", err)
		}
	})
}

// countingTransport counts requests by method and path.
type countingTransport struct {
	mu    sync.Mutex
	calls map[string]int
}

func (c *countingTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	c.mu.Lock()
	c.calls[req.Method+" "+strings.TrimSuffix(req.URL.Path, "/")]++
	c.mu.Unlock()
	return http.DefaultTransport.RoundTrip(req)
}

func (c *countingTransport) count(method, suffix string) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	n := 0
	for k, v := range c.calls {
		if strings.HasPrefix(k, method+" ") && strings.HasSuffix(k, suffix) {
			n += v
		}
	}
	return n
}

func TestCollections(t *testing.T) {
	tag := newTag()
	counter := &countingTransport{calls: map[string]int{}}
	client := withToken(t, salesforce.WithRoundTripper(counter))

	records := make([]accountRecord, 450)
	for i := range records {
		records[i] = accountRecord{Name: fmt.Sprintf("%s bulk %04d", tag, i), Key: fmt.Sprintf("%s-%04d", tag, i)}
	}
	res, err := client.InsertCollection("Account", records, 200)
	if err != nil {
		t.Fatal(err)
	}
	ids := make([]string, 0, len(res.Results))
	for _, r := range res.Results {
		ids = append(ids, r.Id)
	}
	track(t, ids...)
	if res.HasSalesforceErrors || len(res.Results) != 450 {
		t.Fatalf("%d results, errors: %v", len(res.Results), res.HasSalesforceErrors)
	}
	if n := counter.count("POST", "/composite/sobjects"); n != 3 {
		t.Errorf("450 records in batches of 200 took %d calls, want 3", n)
	}
	var count []map[string]any
	if err := client.Query("SELECT Id FROM Account WHERE Name LIKE '"+tag+" bulk%'", &count); err != nil || len(count) != 450 {
		t.Fatalf("found %d records: %v", len(count), err)
	}

	t.Run("per-record errors", func(t *testing.T) {
		res, err := client.InsertCollection("Account", []accountRecord{{Name: tag + " ok"}, {Tier: "Gold"}}, 200)
		if err != nil {
			t.Fatal(err)
		}
		for _, r := range res.Results {
			if r.Success {
				track(t, r.Id)
			}
		}
		if !res.HasSalesforceErrors || !res.Results[0].Success || res.Results[1].Success {
			t.Fatalf("results %+v", res)
		}
		if e := res.Results[1].Errors[0]; e.StatusCode != "REQUIRED_FIELD_MISSING" || strings.Join(e.Fields, ",") != "Name" {
			t.Errorf("error %+v", e)
		}
	})

	t.Run("update, upsert and delete", func(t *testing.T) {
		updates := []accountRecord{{Id: ids[0], Tier: "Gold"}, {Id: ids[1], Tier: "Silver"}}
		if res, err := client.UpdateCollection("Account", updates, 200); err != nil || res.HasSalesforceErrors {
			t.Fatal(err, res)
		}
		upserts := []accountRecord{{Key: fmt.Sprintf("%s-%04d", tag, 2), Tier: "Bronze"}, {Key: tag + "-new", Name: tag + " bulk new"}}
		res, err := client.UpsertCollection("Account", "External_Key__c", upserts, 200)
		if err != nil || res.HasSalesforceErrors || len(res.Results) != 2 {
			t.Fatal(err, res)
		}
		if res.Results[0].Id != ids[2] {
			t.Errorf("upserting an existing key returned %s, want %s", res.Results[0].Id, ids[2])
		}
		track(t, res.Results[1].Id)
		var tiers []accountRecord
		if err := client.Query(fmt.Sprintf("SELECT Tier__c FROM Account WHERE Id IN ('%s','%s','%s') ORDER BY Name", ids[0], ids[1], ids[2]), &tiers); err != nil {
			t.Fatal(err)
		}
		if len(tiers) != 3 || tiers[0].Tier != "Gold" || tiers[1].Tier != "Silver" || tiers[2].Tier != "Bronze" {
			t.Errorf("tiers %+v", tiers)
		}

		deletes := []accountRecord{{Id: ids[0]}, {Id: ids[1]}}
		if res, err := client.DeleteCollection("Account", deletes, 200); err != nil || res.HasSalesforceErrors {
			t.Fatal(err, res)
		}
		res, err = client.DeleteCollection("Account", deletes[:1], 200)
		if err != nil || !res.HasSalesforceErrors || res.Results[0].Errors[0].StatusCode != "ENTITY_IS_DELETED" {
			t.Fatalf("deleting again: %v %+v", err, res)
		}
	})
}

func TestCompositeHelpers(t *testing.T) {
	tag := newTag()
	counter := &countingTransport{calls: map[string]int{}}
	client := withToken(t, salesforce.WithRoundTripper(counter))

	records := make([]accountRecord, 250)
	for i := range records {
		records[i] = accountRecord{Name: fmt.Sprintf("%s comp %04d", tag, i), Key: fmt.Sprintf("%s-c%04d", tag, i)}
	}
	res, err := client.InsertComposite("Account", records, 200, true)
	if err != nil {
		t.Fatal(err)
	}
	ids := make([]string, 0, len(res.Results))
	for _, r := range res.Results {
		ids = append(ids, r.Id)
	}
	track(t, ids...)
	if res.HasSalesforceErrors || len(res.Results) != 250 {
		t.Fatalf("%d results, errors: %v", len(res.Results), res.HasSalesforceErrors)
	}
	if n := counter.count("POST", "/composite"); n != 1 {
		t.Errorf("one composite call expected, got %d", n)
	}

	t.Run("allOrNone rolls back every subrequest", func(t *testing.T) {
		bad := []accountRecord{{Name: tag + " rollback 1"}, {Name: tag + " rollback 2", Tier: "Platinum"}}
		res, err := client.InsertComposite("Account", bad, 1, true)
		if err != nil {
			t.Fatal(err)
		}
		for _, r := range res.Results {
			if r.Success {
				track(t, r.Id)
			}
		}
		if !res.HasSalesforceErrors {
			t.Errorf("expected errors: %+v", res)
		}
		var found []map[string]any
		if err := client.Query("SELECT Id FROM Account WHERE Name LIKE '"+tag+" rollback%'", &found); err != nil || len(found) != 0 {
			t.Errorf("found %d rolled-back records: %v", len(found), err)
		}
	})

	t.Run("update, upsert and delete", func(t *testing.T) {
		if res, err := client.UpdateComposite("Account", []accountRecord{{Id: ids[0], Tier: "Gold"}}, 200, true); err != nil || res.HasSalesforceErrors {
			t.Fatal(err, res)
		}
		up := []accountRecord{{Key: fmt.Sprintf("%s-c%04d", tag, 1), Tier: "Silver"}}
		if res, err := client.UpsertComposite("Account", "External_Key__c", up, 200, true); err != nil || res.HasSalesforceErrors || res.Results[0].Id != ids[1] {
			t.Fatal(err, res)
		}
		var tiers []accountRecord
		if err := client.Query(fmt.Sprintf("SELECT Tier__c FROM Account WHERE Id IN ('%s','%s') ORDER BY Name", ids[0], ids[1]), &tiers); err != nil {
			t.Fatal(err)
		}
		if len(tiers) != 2 || tiers[0].Tier != "Gold" || tiers[1].Tier != "Silver" {
			t.Errorf("tiers %+v", tiers)
		}
		del := make([]accountRecord, len(ids))
		for i, id := range ids {
			del[i] = accountRecord{Id: id}
		}
		if res, err := client.DeleteComposite("Account", del, 200, true); err != nil || res.HasSalesforceErrors {
			t.Fatal(err, res)
		}
		var left []map[string]any
		if err := client.Query("SELECT Id FROM Account WHERE Name LIKE '"+tag+" comp%'", &left); err != nil || len(left) != 0 {
			t.Errorf("%d records left: %v", len(left), err)
		}
	})
}

func TestSessionRefresh(t *testing.T) {
	needsPassword(t)
	client, err := salesforce.Init(passwordCreds(os.Getenv("SF_USERNAME")))
	if err != nil {
		t.Fatal(err)
	}
	old := client.GetAccessToken()
	resp, err := http.PostForm(loginURL+"/services/oauth2/revoke", url.Values{"token": {old}})
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("revoke: %s", resp.Status)
	}
	// The next call gets INVALID_SESSION_ID, logs in again and retries.
	var users []map[string]any
	if err := client.Query("SELECT Id FROM User LIMIT 1", &users); err != nil {
		t.Fatal(err)
	}
	if client.GetAccessToken() == old {
		t.Error("the token wasn't refreshed")
	}
	if len(users) != 1 {
		t.Errorf("users %v", users)
	}
}

func TestCompression(t *testing.T) {
	tag := newTag()
	client := withToken(t, salesforce.WithCompressionHeaders(true))
	created, err := client.InsertOne("Account", accountRecord{Name: tag + " gzip"})
	if err != nil {
		t.Fatal(err)
	}
	track(t, created.Id)
	var accts []accountRecord
	if err := client.Query("SELECT Name FROM Account WHERE Id = '"+created.Id+"'", &accts); err != nil {
		t.Fatal(err)
	}
	if len(accts) != 1 || accts[0].Name != tag+" gzip" {
		t.Errorf("got %+v", accts)
	}
}
