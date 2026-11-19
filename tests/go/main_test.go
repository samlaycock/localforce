// End-to-end tests driving localforce with go-salesforce
// (github.com/k-capehart/go-salesforce), the most used Go client for
// Salesforce's REST API.
//
// TestMain starts target/debug/localforce (or LOCALFORCE_BIN) on free ports
// with the conformance fixtures, and stops it afterwards. To run against a
// real org instead, set SF_INSTANCE_URL + SF_ACCESS_TOKEN, or SF_LOGIN_URL +
// SF_USERNAME + SF_PASSWORD (+ SF_SECURITY_TOKEN) + SF_CLIENT_ID +
// SF_CLIENT_SECRET, which the login tests also need. See tests/README.md.
package e2e

import (
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/k-capehart/go-salesforce/v3"
)

// apiVersion is go-salesforce's default. Don't change it: the SDK's query
// paging trims a hard-coded "/services/data/v63.0" from nextRecordsUrl.
const apiVersion = "63.0"

var (
	// repoRoot is the localforce repository.
	repoRoot string
	// isLocalforce is false when running against a real org.
	isLocalforce = true
	// loginURL is where the OAuth token endpoint is.
	loginURL string
	// sf is the shared client every conformance case runs through.
	sf *salesforce.Salesforce
	// userID is the session user's Id; user2ID another active user's, or "".
	userID, user2ID string
)

func init() {
	_, file, _, _ := runtime.Caller(0)
	repoRoot = filepath.Join(filepath.Dir(file), "..", "..")
}

func TestMain(m *testing.M) {
	os.Exit(run(m))
}

func run(m *testing.M) int {
	if os.Getenv("SF_INSTANCE_URL") != "" || os.Getenv("SF_LOGIN_URL") != "" {
		isLocalforce = false
		loginURL = envOr("SF_LOGIN_URL", os.Getenv("SF_INSTANCE_URL"))
	} else {
		stop, err := startLocalforce()
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
		defer stop()
	}
	var err error
	sf, err = connect()
	if err != nil {
		fmt.Fprintln(os.Stderr, "connecting:", err)
		return 1
	}
	if userID, user2ID, err = userIDs(sf); err != nil {
		fmt.Fprintln(os.Stderr, "finding users:", err)
		return 1
	}
	return m.Run()
}

func envOr(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}

func freePort() (int, error) {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0, err
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port, nil
}

// startLocalforce starts the server with the fixtures and points the SF_*
// variables at it; it returns a function that stops it.
func startLocalforce() (func(), error) {
	bin := envOr("LOCALFORCE_BIN", filepath.Join(repoRoot, "target", "debug", "localforce"))
	if _, err := os.Stat(bin); err != nil {
		return nil, fmt.Errorf("no localforce binary at %s: run `cargo build` first, or set LOCALFORCE_BIN", bin)
	}
	port, err := freePort()
	if err != nil {
		return nil, err
	}
	tlsPort, err := freePort()
	if err != nil {
		return nil, err
	}
	dataDir, err := os.MkdirTemp("", "localforce-go-")
	if err != nil {
		return nil, err
	}
	fixtures := filepath.Join(repoRoot, "tests", "conformance", "fixtures")
	log, err := os.Create(filepath.Join(dataDir, "localforce.log"))
	if err != nil {
		return nil, err
	}
	cmd := exec.Command(bin,
		"--host", "127.0.0.1",
		"--port", fmt.Sprint(port),
		"--tls-port", fmt.Sprint(tlsPort),
		"--data-dir", dataDir,
		"--config", filepath.Join(fixtures, "localforce.toml"),
		"--metadata", filepath.Join(fixtures, "sfdx"),
		// Like Salesforce, refuse tokens localforce didn't issue.
		"--strict-sessions",
	)
	cmd.Stdout, cmd.Stderr = log, log
	cmd.Env = append(os.Environ(), "NO_COLOR=1")
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	stop := func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		_ = log.Close()
		_ = os.RemoveAll(dataDir)
	}
	base := fmt.Sprintf("http://127.0.0.1:%d", port)
	deadline := time.Now().Add(30 * time.Second)
	for {
		resp, err := http.Get(base + "/services/data")
		if err == nil {
			resp.Body.Close()
			break
		}
		if time.Now().After(deadline) {
			stop()
			return nil, fmt.Errorf("localforce didn't start on %s", base)
		}
		time.Sleep(50 * time.Millisecond)
	}
	loginURL = base
	for k, v := range map[string]string{
		"SF_LOGIN_URL":       base,
		"SF_USERNAME":        "admin@localforce.test",
		"SF_PASSWORD":        "any-password",
		"SF_SECURITY_TOKEN":  "any-token",
		"SF_CLIENT_ID":       "localforce-tests",
		"SF_CLIENT_SECRET":   "any-secret",
		"LF_SECOND_USERNAME": "second@localforce.test",
		"LF_DEV_API":         base + "/__localforce/api",
	} {
		os.Setenv(k, v)
	}
	return stop, nil
}

// hasPassword reports whether the target can log in with a username and
// password (rather than only a token).
func hasPassword() bool {
	return os.Getenv("SF_USERNAME") != "" && os.Getenv("SF_PASSWORD") != "" &&
		os.Getenv("SF_CLIENT_ID") != "" && os.Getenv("SF_CLIENT_SECRET") != ""
}

// connect logs in as the test user: with an access token for a real org,
// otherwise with the username-password flow.
func connect() (*salesforce.Salesforce, error) {
	if !isLocalforce && os.Getenv("SF_ACCESS_TOKEN") != "" {
		return salesforce.Init(salesforce.Creds{
			Domain:      os.Getenv("SF_INSTANCE_URL"),
			AccessToken: os.Getenv("SF_ACCESS_TOKEN"),
		})
	}
	return salesforce.Init(salesforce.Creds{
		Domain:         loginURL,
		Username:       os.Getenv("SF_USERNAME"),
		Password:       os.Getenv("SF_PASSWORD"),
		SecurityToken:  os.Getenv("SF_SECURITY_TOKEN"),
		ConsumerKey:    os.Getenv("SF_CLIENT_ID"),
		ConsumerSecret: os.Getenv("SF_CLIENT_SECRET"),
	})
}

// userIDs finds the session user, and the second user from LF_SECOND_USERNAME.
func userIDs(client *salesforce.Salesforce) (string, string, error) {
	res, err := rawRequest(client, "GET", "/services/oauth2/userinfo", nil, nil)
	if err != nil {
		return "", "", err
	}
	info, _ := res.body.(map[string]any)
	user, _ := info["user_id"].(string)
	if user == "" {
		return "", "", fmt.Errorf("userinfo has no user_id: %v", res.body)
	}
	second := os.Getenv("LF_SECOND_USERNAME")
	if second == "" {
		return user, "", nil
	}
	var users []map[string]any
	soql := fmt.Sprintf("SELECT Id FROM User WHERE Username = '%s' AND IsActive = true", second)
	if err := client.Query(soql, &users); err != nil {
		return "", "", err
	}
	if len(users) == 0 {
		return user, "", nil
	}
	id, _ := users[0]["Id"].(string)
	return user, id, nil
}

// response is a plain HTTP response with its body decoded as JSON (or kept
// as text when it isn't JSON).
type response struct {
	status  int
	headers http.Header
	body    any
}

// rawRequest is a plain HTTP call with the client's session token. A path
// starting with /services is relative to the instance URL; anything else to
// /services/data/vXX.X. headers override the defaults (an empty
// Authorization sends none).
func rawRequest(client *salesforce.Salesforce, method, path string, body io.Reader, headers map[string]string) (*response, error) {
	u := client.GetInstanceUrl()
	if len(path) >= 9 && path[:9] == "/services" {
		u += path
	} else {
		u += "/services/data/v" + apiVersion + path
	}
	req, err := http.NewRequest(method, u, body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+client.GetAccessToken())
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	for k, v := range headers {
		if v == "" {
			req.Header.Del(k)
		} else {
			req.Header.Set(k, v)
		}
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	out := &response{status: resp.StatusCode, headers: resp.Header}
	if len(data) > 0 {
		if json.Unmarshal(data, &out.body) != nil {
			out.body = string(data)
		}
	}
	return out, nil
}

// queryEscape escapes SOQL for a URL.
func queryEscape(soql string) string { return url.QueryEscape(soql) }
