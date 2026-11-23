# lineup-gateway

The gateway binary, outside the `ghcr.io/alexey-lapin/lineup-gateway` image. Full docs:
https://github.com/alexey-lapin/lineup/blob/main/docs/gateway.md

Files:

- `lineup-gateway`: the binary. The Linux builds are static (musl) and run on any distribution.
- `gateway.example.toml`: the image's config, a starting point.
- `gateway-tools-requirements.txt`: the yt-dlp and streamlink versions the image ships, with every
  package they use, pinned with hashes.

Relay rules need the extractors their config names; acestream and link rules need none. The
example runs them from a Python virtual environment at `/opt/tools`, as the image does:

```sh
python3 -m venv /opt/tools
/opt/tools/bin/pip install --no-deps --require-hashes -r gateway-tools-requirements.txt
/opt/tools/bin/pip check
```

The pins need Python 3.15, on Linux or macOS. Installed somewhere else, point each
`[extractors.*] command` there (a bare name such as `yt-dlp` is looked up on PATH).

Then make a config from the example, check it and run it:

```sh
cp gateway.example.toml gateway.toml
./lineup-gateway --check-config --config gateway.toml
./lineup-gateway --config gateway.toml
```

It needs the system's CA certificates for HTTPS. No authentication: run it on a trusted LAN only.
