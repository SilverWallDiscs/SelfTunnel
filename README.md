# SelfTunnel

SelfTunnel exposes a service running on a private machine through a public SelfServer relay. The private machine initiates and maintains the outbound connection, so it does not need an inbound port forward.

## Components

- **SelfTunnel** — Windows desktop client. Stores the SelfServer address and access token, maintains the outbound tunnel, and can optionally start with Windows and reconnect automatically.
- **SelfServer** — Linux relay daemon intended for a small public VPS such as an Oracle Cloud VM. Accepts authenticated client sessions and public TCP connections, then forwards each connection through the tunnel to the private machine.

## Initial target

The first supported service is Minecraft Java Edition (TCP).

- Public Minecraft listener: TCP port `25565`
- SelfServer control listener: TCP port `9000` (configurable)
- Local Minecraft destination: `127.0.0.1:25565` (configurable)

The public listener and control listener are separate. Only the SelfServer needs publicly reachable ports. SelfTunnel makes an outbound connection to the control listener.

## Security

Use a long random access token and keep it private. Do not expose the control port without authentication. The relay must apply connection limits and timeouts. TLS protection for the control connection is part of the production implementation; do not treat an unauthenticated development build as safe for public use.

## Repository layout

```
crates/
  selfserver/   Linux relay daemon
  selftunnel/   Windows desktop client
```

## Build status

This repository is being initialized. The first milestone is a working authenticated tunnel between one SelfTunnel client and one SelfServer instance, followed by multiple simultaneous Minecraft player connections, Windows auto-start, and reconnect-on-launch.

## Development

Requires a recent stable Rust toolchain.

```sh
cargo check --workspace
```
