import{_ as a,o as c,c as o,a0 as t}from"./chunks/framework.CNXp-E7Q.js";const _=JSON.parse('{"title":"How-to: sccache backend","description":"","frontmatter":{},"headers":[],"relativePath":"how-to/sccache-backend.md","filePath":"how-to/sccache-backend.md"}'),r={name:"how-to/sccache-backend.md"};function n(d,e,s,h,i,l){return c(),o("div",null,[...e[0]||(e[0]=[t(`<h1 id="how-to-sccache-backend" tabindex="-1">How-to: sccache backend <a class="header-anchor" href="#how-to-sccache-backend" aria-label="Permalink to &quot;How-to: sccache backend&quot;">​</a></h1><p>sccache&#39;s backend is decoupled via an env file — set <code>runner.sccache_conf</code> to a <code>KEY=VAL</code> file (mounted into containers as a secret). Without it, sccache uses a local cache volume.</p><h2 id="local-s3-garage" tabindex="-1">Local S3 (garage) <a class="header-anchor" href="#local-s3-garage" aria-label="Permalink to &quot;Local S3 (garage)&quot;">​</a></h2><pre><code>docker run -d --name garage --restart always \\
  -v $HOME/bupha-data/garage:/data \\
  -v $HOME/bupha-data/garage.toml:/etc/garage.toml \\
  -p 9100:9100 dxflrs/garage:v1.x.x
</code></pre><p>Then in the garage CLI: <code>bucket create bupha-ci</code> → <code>key create sccache</code> → <code>bucket allow --read --write --owner bupha-ci --key sccache</code>.</p><p>Env file:</p><pre><code>SCCACHE_ENDPOINT=http://&lt;docker-gateway&gt;:9100
SCCACHE_BUCKET=bupha-ci
SCCACHE_REGION=garage
SCCACHE_S3_USE_SSL=false
AWS_ACCESS_KEY_ID=...
AWS_SECRET_ACCESS_KEY=...
</code></pre><h2 id="migrating-to-hosted-s3-r2-aws" tabindex="-1">Migrating to hosted S3 (R2 / AWS) <a class="header-anchor" href="#migrating-to-hosted-s3-r2-aws" aria-label="Permalink to &quot;Migrating to hosted S3 (R2 / AWS)&quot;">​</a></h2><p>Same file, new values — <code>SCCACHE_ENDPOINT</code> to the provider URL, real region, new creds. No code changes; that&#39;s the point of the env file.</p>`,9)])])}const p=a(r,[["render",n]]);export{_ as __pageData,p as default};
