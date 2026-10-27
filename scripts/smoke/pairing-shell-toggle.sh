#!/usr/bin/env bash
# pairing terminal-access toggle: daemon-half smoke on the committed server blob
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BLOB="${YMUX_SERVER_BLOB:-$ROOT/app/src-tauri/resources/ymux-server-linux-x64}"
D=""; DPID=""

cleanup() {
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null && wait "$DPID" 2>/dev/null
  [ -n "$D" ] && rm -rf "$D"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

setup_fail() { echo "RESULT FAIL setup=$1"; exit 2; }

for t in tmux jq curl python3; do
  command -v "$t" >/dev/null 2>&1 || setup_fail "tool-$t"
done
[ -f "$BLOB" ] || setup_fail "blob-missing"

D="$(mktemp -d -t pairing-smoke.XXXXXX)" || setup_fail "mktemp"
mkdir -p "$D/data" "$D/files"
install -m755 "$BLOB" "$D/ymux-server" || setup_fail "blob-install"
PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')" || setup_fail "port"
BASE="http://127.0.0.1:$PORT"

"$D/ymux-server" serve --port "$PORT" --dir "$D/data" --files-root "$D/files" >"$D/daemon.log" 2>&1 &
DPID=$!

VER_JSON=""
for _ in $(seq 1 30); do
  VER_JSON="$(curl -s -m 2 "$BASE/api/version")" && [ -n "$VER_JSON" ] && break
  kill -0 "$DPID" 2>/dev/null || setup_fail "daemon-exited"
  sleep 0.5
done
[ -n "$VER_JSON" ] || setup_fail "daemon-timeout"
VER="$(printf '%s' "$VER_JSON" | jq -r '[.. | strings | select(test("^v?[0-9]+\\.[0-9]+\\.[0-9]+"))][0] // empty' | sed 's/^v//')"
[ -n "$VER" ] || setup_fail "version-unparsed"
[ "$(printf '%s\n%s\n' 2.13.0 "$VER" | sort -V | head -n1)" = "2.13.0" ] || setup_fail "version-$VER-below-2.13.0"
echo "SETUP version=$VER tools=ok"

# owner token file appears after start
for _ in $(seq 1 20); do [ -s "$D/data/token" ] && break; sleep 0.25; done
[ -s "$D/data/token" ] || setup_fail "owner-token-missing"
OWNER="$(tr -d '\r\n' <"$D/data/token")"
# header files keep tokens out of argv and stdout
printf 'Authorization: Bearer %s\n' "$OWNER" >"$D/h-owner"; chmod 600 "$D/h-owner"

# req <header-file|-> METHOD PATH [json body on stdin] -> sets CODE, BODY
req() {
  local h="$1" m="$2" p="$3"
  local args=(-s -m 10 -o "$D/body" -w '%{http_code}' -X "$m" "$BASE$p")
  [ "$h" != "-" ] && args+=(-H "@$h")
  if [ "$m" = PUT ] || [ "$m" = POST ]; then
    args+=(-H 'Content-Type: application/json' --data-binary @-)
    CODE="$(curl "${args[@]}")"
  else
    CODE="$(curl "${args[@]}" </dev/null)"
  fi
  BODY="$(cat "$D/body")"
}
FAILED=0
fail() { echo "$1 FAIL $2"; FAILED=1; }

# issue_redeem <name> -> ID, OTT, DEVTOK (device bearer file $D/h-dev-$name)
issue() {
  req "$D/h-owner" POST /api/pairing/issue <<<"{\"device_name\":\"$1\"}"
  ID="$(printf '%s' "$BODY" | jq -r '.device_id // empty')"
  OTT="$(printf '%s' "$BODY" | jq -r '.one_shot_token // empty')"
  [ "$CODE" = 200 ] && [ -n "$ID" ] && [ -n "$OTT" ] || setup_fail "issue-$1-http-$CODE"
}
redeem() {
  req - POST /api/pairing/redeem <<<"$(jq -nc --arg t "$OTT" '{one_shot_token:$t}')"
  DEVTOK="$(printf '%s' "$BODY" | jq -r '.long_term_token // empty')"
  [ "$CODE" = 200 ] && [ -n "$DEVTOK" ] || setup_fail "redeem-$1-http-$CODE"
  printf 'Authorization: Bearer %s\n' "$DEVTOK" >"$D/h-dev-$1"; chmod 600 "$D/h-dev-$1"
}
dev_status() { # <id> <field>
  req "$D/h-owner" GET /api/pairing/devices
  printf '%s' "$BODY" | jq -r --arg id "$1" --arg f "$2" '.devices[] | select(.device_id==$id) | .[$f] | if type=="string" then . else tojson end'
}
gate_codes() { # <header-file> -> SET_CODE TERM_CODE
  req "$1" GET /api/v2/settings; SET_CODE="$CODE"
  req "$1" GET /api/v2/term/sessions; TERM_CODE="$CODE"
}
# set_shell <id> add|remove -> mirror of mobile_pairing_set_shell: GET -> edit -> PUT
set_shell() {
  req "$D/h-owner" GET "/api/v2/devices/$1/scopes"
  local cur="$BODY" new
  if [ "$2" = add ]; then
    new="$(printf '%s' "$cur" | jq -c '{scopes: ((.scopes + ["shell:attach"]) | unique)}')"
  else
    new="$(printf '%s' "$cur" | jq -c '{scopes: (.scopes - ["shell:attach"])}')"
  fi
  req "$D/h-owner" PUT "/api/v2/devices/$1/scopes" <<<"$new"
  PUT_CODE="$CODE"
  STORED_SHELL="$(printf '%s' "$BODY" | jq -r '(.scopes // []) | index("shell:attach") != null')"
}

issue A; A_ID="$ID"; A_OTT="$OTT"; redeem A; A_TOK="$DEVTOK"

# O1: fresh device active, stored "all", no shell
STATUS="$(dev_status "$A_ID" status)"; SC="$(dev_status "$A_ID" scopes)"
case "$SC" in all) LS=all;; *) LS=list;; esac
case "$SC" in *shell:attach*) HAS=true;; *) HAS=false;; esac
if [ "$STATUS" = active ] && [ "$LS" = all ] && [ "$HAS" = false ]; then
  echo "O1 PASS status=$STATUS list_scopes=$LS has_shell=$HAS"
else fail O1 "status=$STATUS list_scopes=$LS has_shell=$HAS"; fi

# O2: grant
set_shell "$A_ID" add; P2="$PUT_CODE"; S2="$STORED_SHELL"
gate_codes "$D/h-dev-A"
if [ "$P2" = 200 ] && [ "$S2" = true ] && [ "$SET_CODE" = 200 ] && [ "$TERM_CODE" = 200 ]; then
  echo "O2 PASS put=$P2 stored_shell=$S2 settings=$SET_CODE term_list=$TERM_CODE"
else fail O2 "put=$P2 stored_shell=$S2 settings=$SET_CODE term_list=$TERM_CODE"; fi

# O3: withdraw
set_shell "$A_ID" remove; P3="$PUT_CODE"; S3="$STORED_SHELL"
gate_codes "$D/h-dev-A"
if [ "$P3" = 200 ] && [ "$S3" = false ] && [ "$SET_CODE" = 403 ] && [ "$TERM_CODE" = 403 ]; then
  echo "O3 PASS put=$P3 stored_shell=$S3 settings=$SET_CODE term_list=$TERM_CODE"
else fail O3 "put=$P3 stored_shell=$S3 settings=$SET_CODE term_list=$TERM_CODE"; fi

# O4: pending and revoked devices reject PUT
issue B; B_ID="$ID"; B_OTT="$OTT"
req "$D/h-owner" PUT "/api/v2/devices/$B_ID/scopes" <<<'{"scopes":["shell:attach"]}'; PEND="$CODE"
req "$D/h-owner" DELETE "/api/pairing/devices/$A_ID"
req "$D/h-owner" PUT "/api/v2/devices/$A_ID/scopes" <<<'{"scopes":["shell:attach"]}'; REV="$CODE"
if [ "$PEND" = 404 ] && [ "$REV" = 404 ]; then echo "O4 PASS pending_put=$PEND revoked_put=$REV"
else fail O4 "pending_put=$PEND revoked_put=$REV"; fi

# O5: no token value in daemon output or logs
sleep 0.3
HITS=0; LINES=0
FILES=("$D/daemon.log")
while IFS= read -r f; do FILES+=("$f"); done < <(find "$D/data" "$D/files" -name '*.log' -type f 2>/dev/null)
for f in "${FILES[@]}"; do LINES=$((LINES + $(wc -l <"$f"))); done
for v in "$OWNER" "$A_OTT" "$A_TOK" "$B_OTT"; do
  [ -n "$v" ] || continue
  for f in "${FILES[@]}"; do HITS=$((HITS + $(grep -cF -- "$v" "$f"))); done
done
if [ "$HITS" = 0 ]; then echo "O5 PASS token_hits=$HITS log_lines=$LINES"
else fail O5 "token_hits=$HITS log_lines=$LINES"; fi

if [ "$FAILED" = 0 ]; then echo "RESULT PASS"; exit 0; fi
echo "RESULT FAIL"; exit 1
