# Local S3 cache backend (garage)

sccache's backend is decoupled via `~/.config/handl-ci/sccache-env`
(KEY=VAL file passed to the pipeline as a secret). Current backend:
a local **garage** container (disk-backed; redis was dropped because
it is memory-first on an 11 GiB box).

    docker run -d --name handl-garage --restart always \
      -p 9100:3900 -p 9102:3902 \
      -v $HOME/handl-ci-data/garage:/data \
      -v $HOME/handl-ci-data/garage.toml:/etc/garage.toml \
      dxflrs/garage:v1.1.0

Bootstrap: `garage layout assign -z dc1 -c 1G <node>` → `layout apply`
→ `bucket create handl-ci` → `key create sccache` →
`bucket allow --read --write --owner handl-ci --key sccache`.

sccache-env: `SCCACHE_ENDPOINT=http://172.17.0.1:9100`,
`SCCACHE_BUCKET=handl-ci`, `SCCACHE_REGION=garage`, AWS_* creds from
`garage key info sccache`. To move to R2/AWS later, change only the
env file — endpoint + creds, same protocol.

NB: AIStor's minio binaries are license-gated now (server refuses
bucket ops without `mc license update`) — use garage or an older
community minio instead.
