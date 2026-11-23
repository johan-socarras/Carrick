#!/bin/sh
# Starts iroh-relay for Carrick with a certificate:
#
#   RELAY_HOSTNAME  the name or IP address Carrick will use to reach this
#                   relay (required), e.g. relay.example.com or 192.168.1.50
#   RELAY_CERT      "own" (default): a private certificate authority is made
#                   on the first start, in /data/certs. Give its ca.crt to
#                   every computer that uses the relay (--relay-ca). Works
#                   inside a company network, with no domain and no Internet.
#                   "letsencrypt": a public certificate from Let's Encrypt.
#                   Needs a domain pointing here and ports 80 and 443 open.
#   RELAY_EMAIL     contact address for Let's Encrypt (with "letsencrypt").
set -eu

HOST="${RELAY_HOSTNAME:?set RELAY_HOSTNAME to the name or IP this relay is reached at}"
MODE="${RELAY_CERT:-own}"
DATA=/data
mkdir -p "$DATA"

is_ip() {
  case "$1" in
    *:*) return 0 ;;                       # IPv6
    *[!0-9.]*) return 1 ;;                 # has letters: a name
    *) return 0 ;;
  esac
}

case "$MODE" in
  own)
    CERTS="$DATA/certs"
    mkdir -p "$CERTS"
    if [ ! -f "$CERTS/ca.crt" ]; then
      echo "Making a private certificate authority in $CERTS"
      openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
        -keyout "$CERTS/ca.key" -out "$CERTS/ca.crt" -subj "/CN=Carrick relay CA" 2>/dev/null
      chmod 600 "$CERTS/ca.key"
    fi
    # The relay's own certificate is made again whenever the name changes.
    if [ ! -f "$CERTS/default.crt" ] || [ "$(cat "$CERTS/hostname" 2>/dev/null)" != "$HOST" ]; then
      echo "Making the relay certificate for $HOST"
      if is_ip "$HOST"; then san="IP:$HOST"; else san="DNS:$HOST"; fi
      openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
        -keyout "$CERTS/default.key" -out "$CERTS/relay.csr" -subj "/CN=$HOST" 2>/dev/null
      printf 'subjectAltName=%s\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n' "$san" >"$CERTS/ext.cnf"
      openssl x509 -req -in "$CERTS/relay.csr" -CA "$CERTS/ca.crt" -CAkey "$CERTS/ca.key" \
        -CAcreateserial -days 825 -extfile "$CERTS/ext.cnf" -out "$CERTS/default.crt" 2>/dev/null
      chmod 600 "$CERTS/default.key"
      echo "$HOST" >"$CERTS/hostname"
    fi
    TLS="cert_mode = \"Manual\"
cert_dir = \"$CERTS\""
    echo "Carrick users need: --relay https://$HOST/ --relay-ca ca.crt (copy $CERTS/ca.crt to them)"
    ;;
  letsencrypt)
    EMAIL="${RELAY_EMAIL:?set RELAY_EMAIL for Let's Encrypt}"
    mkdir -p "$DATA/letsencrypt"
    TLS="cert_mode = \"LetsEncrypt\"
cert_dir = \"$DATA/letsencrypt\"
contact = \"$EMAIL\""
    echo "Carrick users need: --relay https://$HOST/"
    ;;
  *)
    echo "RELAY_CERT must be \"own\" or \"letsencrypt\", not \"$MODE\"" >&2
    exit 2
    ;;
esac

cat >"$DATA/relay.toml" <<EOF
http_bind_addr = "[::]:80"
enable_relay = true
enable_quic_addr_discovery = true
enable_metrics = false

[tls]
https_bind_addr = "[::]:443"
quic_bind_addr = "[::]:7842"
hostname = "$HOST"
$TLS
EOF

exec iroh-relay --config-path "$DATA/relay.toml"
