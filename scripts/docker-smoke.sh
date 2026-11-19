#!/bin/sh
# Start a localforce image the way the README shows, log in over OAuth, and
# create and query a record over HTTP and HTTPS.
# Usage: scripts/docker-smoke.sh [image]   (default: localforce:smoke)
set -eu
image=${1:-localforce:smoke}
port=${PORT:-9630}
tls_port=${TLS_PORT:-9673}
name=localforce-smoke-$$
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT

docker run -d --name "$name" -p "$port:9600" -p "$tls_port:9643" "$image" >/dev/null

fail() { echo "FAIL: $1"; docker logs "$name" 2>&1 | tail -20; exit 1; }
base=http://localhost:$port
for _ in $(seq 1 50); do
  curl -sf "$base/services/data" >/dev/null 2>&1 && break
  sleep 0.2
done
curl -sf "$base/services/data" | grep -q '"version"' || fail "/services/data didn't answer"

token=$(curl -sf "$base/services/oauth2/token" -d grant_type=password -d client_id=smoke -d client_secret=smoke \
  -d username=admin@localforce.dev -d password=smoke | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
[ -n "$token" ] || fail "the password grant didn't return a token"

api=$base/services/data/v62.0
id=$(curl -sf "$api/sobjects/Account" -H "authorization: Bearer $token" -H 'content-type: application/json' \
  -d '{"Name":"Smoke Test"}' | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
[ -n "$id" ] || fail "creating an Account failed"

curl -sfG "$api/query" -H "authorization: Bearer $token" --data-urlencode "q=SELECT Name FROM Account WHERE Id = '$id'" \
  | grep -q '"Name":"Smoke Test"' || fail "SOQL didn't find the Account"

# HTTPS, with the certificate the image generated.
curl -skf "https://localhost:$tls_port/services/data/v62.0/sobjects/Account/$id" -H "authorization: Bearer $token" \
  | grep -q '"Name":"Smoke Test"' || fail "the Account isn't readable over HTTPS"

curl -sf "$base/" >/dev/null || fail "the web UI didn't answer"
echo "ok: $image answers over HTTP and HTTPS"
