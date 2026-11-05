# Oracle Cloud quick test (Ubuntu ARM64)

This guide runs SelfServer in the foreground for the first end-to-end test. It does not install a permanent systemd service yet.

## 1. Oracle instance and ingress

Create an Ubuntu 24.04 ARM64 (Ampere A1) instance with a public IPv4 address. In the VCN Network Security Group or subnet Security List attached to the instance, add stateful ingress rules:

| Source CIDR | Protocol | Destination port |
|---|---|---:|
| 0.0.0.0/0 | TCP | 9000 |
| 0.0.0.0/0 | TCP | 25565 |

Port 9000 is the encrypted SelfTunnel control connection. Port 25565 is the public Minecraft TCP listener.

SSH into the instance. Verify the architecture:

```sh
uname -m
```

For Oracle Ampere A1 it should print `aarch64`.

## 2. Install build tools and build SelfServer

```sh
sudo apt update
sudo apt install -y build-essential cmake perl pkg-config git curl openssl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

git clone https://github.com/SilverWallDiscs/SelfTunnel.git
cd SelfTunnel
cargo build --release -p selfserver
```

If the repository is already cloned, run `git pull` in the repository before building.

## 3. Make a temporary test CA and a server certificate for your public IPv4

Set `PUBLIC_IP` to the public IPv4 shown on the Oracle instance details page:

```sh
export PUBLIC_IP="YOUR_ORACLE_PUBLIC_IPV4"
mkdir -p "$HOME/selfserver-test"
cd "$HOME/selfserver-test"

# Create a small private test CA.
openssl req -x509 -newkey rsa:3072 -sha256 -days 3650 -nodes \
  -keyout ca.key -out ca.crt \
  -subj "/CN=SelfTunnel Test CA" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"

# Create a server key and certificate request.
openssl req -new -newkey rsa:3072 -sha256 -nodes \
  -keyout server.key -out server.csr \
  -subj "/CN=$PUBLIC_IP"

# The IP SAN is required when the SelfTunnel app connects by IPv4.
printf 'subjectAltName=IP:%s\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' "$PUBLIC_IP" > server.ext

openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out server.crt -days 365 -sha256 -extfile server.ext
chmod 600 ca.key server.key
```

Keep `ca.key` and `server.key` private on Oracle. Copy only `ca.crt` to the Windows PC; select that CA certificate in the SelfTunnel app.

## 4. Open the Linux firewall if it is already enabled

```sh
sudo ufw status
```

If UFW reports `Status: active`, allow the two SelfTunnel ports:

```sh
sudo ufw allow 9000/tcp
sudo ufw allow 25565/tcp
```

Do not enable UFW for this first test unless SSH (normally TCP 22) is already allowed. Oracle's VCN ingress rules are separate from the Linux firewall.

## 5. Start SelfServer in the foreground

Generate a long random token and keep it available to paste into SelfTunnel:

```sh
export SELFTUNNEL_TOKEN="$(openssl rand -hex 32)"
printf 'Copy this token into SelfTunnel: %s\n' "$SELFTUNNEL_TOKEN"

export SELFTUNNEL_CONTROL_BIND="0.0.0.0:9000"
export SELFTUNNEL_PUBLIC_BIND="0.0.0.0:25565"
export SELFTUNNEL_TLS_CERT="$HOME/selfserver-test/server.crt"
export SELFTUNNEL_TLS_KEY="$HOME/selfserver-test/server.key"

cd "$HOME/SelfTunnel"
./target/release/selfserver
```

Keep this SSH terminal open. The startup log should show that control port 9000 and public TCP port 25565 are listening.

In a second SSH terminal, verify the listeners:

```sh
sudo ss -lntp | grep -E ':(9000|25565)\\b'
```

## 6. Copy the CA certificate to Windows

From Windows PowerShell (replace `ubuntu` and the IP if your SSH username differs):

```powershell
scp ubuntu@YOUR_ORACLE_PUBLIC_IPV4:~/selfserver-test/ca.crt "$env:USERPROFILE\Downloads\SelfTunnel-ca.crt"
```

## 7. Build and connect the Windows client

On Windows, with the Rust stable toolchain installed:

```powershell
git clone https://github.com/SilverWallDiscs/SelfTunnel.git
cd SelfTunnel
cargo build --release -p selftunnel
.\target\release\selftunnel.exe
```

In the SelfTunnel window, enter:

- **SelfServer address:** `YOUR_ORACLE_PUBLIC_IPV4:9000`
- **Access token:** the value printed in step 5
- **Local Minecraft server:** `127.0.0.1:25565`
- **Custom CA certificate:** `%USERPROFILE%\Downloads\SelfTunnel-ca.crt`

Click **Connect**. The app should display `Connected — tunnel is ready`, and the Oracle terminal should log `SelfTunnel client authenticated and ready`.

Start your Minecraft Java server on the Windows PC before testing players. It must be reachable locally at `127.0.0.1:25565`.

## 8. Test Minecraft

From the same PC or another computer, add a Minecraft Java server using:

```text
YOUR_ORACLE_PUBLIC_IPV4:25565
```

When a player joins, SelfServer should log `Minecraft connection opened`. When they leave, it should log `Minecraft connection closed`.

PowerShell can test basic TCP reachability (it does not verify the Minecraft protocol):

```powershell
Test-NetConnection YOUR_ORACLE_PUBLIC_IPV4 -Port 9000
Test-NetConnection YOUR_ORACLE_PUBLIC_IPV4 -Port 25565
```

## Troubleshooting

- **TCP test fails:** check both Oracle VCN ingress rules and the Linux firewall. Verify the VM has a public IPv4.
- **TLS certificate validation fails:** select `ca.crt` (not `server.crt`) in the Custom CA field. The server address must use the same IP that was placed in the certificate IP SAN.
- **Token rejected:** compare the app token with the exact token printed by SelfServer. Generate a new one and restart both ends if needed.
- **SelfTunnel says connected, but Minecraft cannot join:** start the Minecraft server first and confirm its local port is `25565`. Check the Local Minecraft server field.
- **No connection log appears when a player joins:** the public TCP port 25565 may not be open in the VCN or OS firewall.

This is an early MVP. Test privately with trusted players. Do not publish the token or the server private key. The foreground server must remain running during the test; stop it with Ctrl+C.
