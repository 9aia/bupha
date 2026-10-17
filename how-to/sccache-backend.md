# How-to: sccache backend

sccache's backend is decoupled via an env file — set
`runner.sccache_conf` to a `KEY=VAL` file (mounted into containers as
a secret). Without it, sccache uses a local cache volume.

## Local S3 (garage)

    docker run -d --name garage --restart always \
      -v $HOME/bupha-data/garage:/data \
      -v $HOME/bupha-data/garage.toml:/etc/garage.toml \
      -p 9100:9100 dxflrs/garage:v1.x.x

Then in the garage CLI: `bucket create bupha-ci` → `key create sccache`
→ `bucket allow --read --write --owner bupha-ci --key sccache`.

Env file:

    SCCACHE_ENDPOINT=http://<docker-gateway>:9100
    SCCACHE_BUCKET=bupha-ci
    SCCACHE_REGION=garage
    SCCACHE_S3_USE_SSL=false
    AWS_ACCESS_KEY_ID=...
    AWS_SECRET_ACCESS_KEY=...

## Migrating to hosted S3 (R2 / AWS)

Same file, new values — `SCCACHE_ENDPOINT` to the provider URL, real
region, new creds. No code changes; that's the point of the env file.
