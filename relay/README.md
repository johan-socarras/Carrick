# A relay of your own

Carrick connects the two computers directly whenever the networks allow it.
When they do not (a strict company firewall, two mobile carriers), the data
goes through a relay. The free public relays only let a trickle through,
so for those networks you can run a relay of your own on a machine you
already have: a server in the office, a home server, a small VM. It costs
nothing beyond that machine, and it is as fast as its connection.

This folder builds one in a container. It is the official `iroh-relay`
server, built from its published source; Carrick adds only the start
script that writes its settings and makes a certificate.

## Run it

On the machine that will be the relay, with Docker:

```sh
cd relay
echo RELAY_HOSTNAME=192.168.1.50 > .env
docker compose up -d --build
```

`RELAY_HOSTNAME` is the name or IP address the other computers use to reach
this machine; Compose reads it from `.env`, so later `docker compose logs`
or `docker compose down` need nothing else. Open TCP 80 and 443 and UDP 7842 to it (on the router too, if
computers outside the network must reach it).

On the first start it makes a private certificate authority in
`data/certs/`. Copy `data/certs/ca.crt` to every computer that uses the
relay, and tell Carrick about both:

```sh
carrick --relay https://192.168.1.50/ --relay-ca ca.crt send ...
carrick --relay https://192.168.1.50/ --relay-ca ca.crt receive ...
```

or set `CARRICK_RELAY` and `CARRICK_RELAY_CA`, which the window reads too.
Keep `data/certs/ca.key` private: whoever has it can impersonate the relay.

### With a domain

If the machine has a public domain name pointing at it, it can take a
public certificate from Let's Encrypt instead, and nobody needs `ca.crt`:

```sh
printf 'RELAY_HOSTNAME=relay.example.com\nRELAY_CERT=letsencrypt\nRELAY_EMAIL=you@example.com\n' > .env
docker compose up -d --build
```

Ports 80 and 443 must be reachable from the Internet for the certificate.

## What a relay sees

Nothing of the files. Carrick encrypts end to end; the relay forwards
encrypted packets between the two computers and learns only that they talk.
Anyone who knows the address can use the relay; keep it inside your network,
or behind your firewall, if that matters.
