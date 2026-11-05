# SelfTunnel

SelfTunnel publishes a TCP service running on a private computer through a public relay. The private computer initiates the connection to SelfServer, so it does not need port forwarding, a public IPv4 address, or IPv6.

The first target is **Minecraft Java Edition**. Players connect to the public SelfServer IPv4 address on port `25565`; they do not install SelfTunnel.

## Components

- **SelfTunnel** — Windows desktop app built with Rust/egui. Saves the SelfServer endpoint, token, local target and optional CA certificate; supports reconnect and optional Windows startup.
- **SelfServer** — Linux relay daemon. Listens for one authenticated SelfTunnel client and accepts multiple public TCP connections.
- **SelfProtocol** — shared framed protocol. Multiple player TCP streams are multiplexed over one TLS connection between SelfTunnel and SelfServer.

## Connection layout

```text
Minecraft players
       |
       | TCP to SelfServer:25565
       v
SelfServer on public IPv4 (Oracle VM)
       |
       | authenticated TLS tunnel on port 9000
       | one persistent outbound connection from the private PC
       v
SelfTunnel on Windows
       |
       | TCP to 127.0.0.1:25565
       v
Minecraft Java server on the private PC
```

Only the Oracle VM needs public ingress. Open TCP ports `9000` (SelfTunnel control/TLS) and `25565` (Minecraft players) in both the cloud firewall/security list and the Linux firewall.

## Build

Install a current stable Rust toolchain.

Build SelfServer on the Linux machine that will run it:

```sh
cargo build --release -p selfserver
```

Build the Windows app on Windows:

```powershell
cargo build --release -p selftunnel
```

Run the shared protocol tests:

```sh
cargo test -p selfprotocol
```

## Configure TLS

SelfTunnel validates the SelfServer certificate. The recommended setup is a DNS name pointing to the Oracle VM and a certificate issued by a public CA such as Let's Encrypt.

For a Let's Encrypt certificate, the server paths are usually:

- `/etc/letsencrypt/live/YOUR_DOMAIN/fullchain.pem`
- `/etc/letsencrypt/live/YOUR_DOMAIN/privkey.pem`

The DNS name entered in SelfTunnel must match the certificate. If using a private/self-signed CA, configure the PEM CA certificate path in the SelfTunnel app; do not disable certificate validation.

Example self-signed certificate for testing (replace the IP and protect the private key):

```sh
openssl req -x509 -newkey rsa:3072 -sha256 -days 365 -nodes \
  -keyout server.key -out server.crt \
  -subj "/CN=YOUR_ORACLE_IPV4" \
  -addext "subjectAltName=IP:YOUR_ORACLE_IPV4" \
  -addext "basicConstraints=critical,CA:TRUE"
```

Copy `server.crt` to the Windows PC and select it in the **Custom CA certificate** field. Keep `server.key` only on the Oracle VM.

## Run SelfServer on Oracle Linux/Ubuntu

Build the Linux binary on the Oracle VM (this also builds for the VM's CPU architecture):

```sh
sudo apt update
sudo apt install -y build-essential cmake perl pkg-config git
git clone https://github.com/SilverWallDiscs/SelfTunnel.git
cd SelfTunnel
cargo build --release -p selfserver
```

Create a secret token:

```sh
openssl rand -hex 32
```

Set the environment on the Oracle VM (use your own token and certificate paths):

```sh
export SELFTUNNEL_TOKEN="PASTE_YOUR_RANDOM_TOKEN"
export SELFTUNNEL_CONTROL_BIND="0.0.0.0:9000"
export SELFTUNNEL_PUBLIC_BIND="0.0.0.0:25565"
export SELFTUNNEL_TLS_CERT="/etc/letsencrypt/live/YOUR_DOMAIN/fullchain.pem"
export SELFTUNNEL_TLS_KEY="/etc/letsencrypt/live/YOUR_DOMAIN/privkey.pem"
./target/release/selfserver
```

For a permanent service, use the example in [deploy/selfserver.service](deploy/selfserver.service) and [deploy/selfserver.env.example](deploy/selfserver.env.example). Store the real environment file outside the repository with restrictive permissions (for example, `chmod 600`); never commit the token or TLS private key.

## Configure SelfTunnel on Windows

1. Start your Minecraft Java server locally. The default destination is `127.0.0.1:25565`.
2. Open SelfTunnel.
3. Set **SelfServer address** to `YOUR_DOMAIN:9000` (or `YOUR_ORACLE_IPV4:9000` for a matching IP certificate).
4. Paste the same `SELFTUNNEL_TOKEN` value configured on SelfServer.
5. Leave **Local Minecraft server** as `127.0.0.1:25565`, unless your server uses another port.
6. Leave **Custom CA certificate** empty for a publicly trusted certificate. For a private CA, select its PEM certificate.
7. Press **Connect**. Enable **Connect automatically** and **Start SelfTunnel with Windows** as needed.

Players join with:

```text
YOUR_DOMAIN:25565
```

## Current MVP scope and limits

- One authenticated SelfTunnel client per SelfServer process.
- Up to 128 simultaneous public TCP connections.
- One configured local TCP destination (default Minecraft Java port 25565).
- TLS encryption and token authentication on the control connection.
- Automatic reconnect and Windows startup registration in the desktop app.
- A reconnecting tunnel is not a substitute for keeping the Minecraft server process running.
- This is an early MVP. Test with a private server and trusted players before exposing it broadly; traffic limits, detailed metrics, protocol fuzzing, update signing and broader abuse protections remain future work.

## Configuration and security

The Windows app stores settings in the user's configuration directory under `SelfTunnel/config.toml`. The access token is stored in that file so automatic reconnect can work. Treat the Windows account and configuration file as sensitive.

Never expose the SelfServer control port without TLS and a strong random token. Do not share the token publicly. The Minecraft listener is a public TCP proxy to the one local destination configured in SelfTunnel.
