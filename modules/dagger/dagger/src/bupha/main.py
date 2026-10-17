"""Shared CI primitives for rust workspaces — one definition, runs anywhere.

Install into a consumer repo:

    dagger install github.com/9aia/bupha/modules/dagger
    # then:  dag.bupha().test(source)  or  dagger call test

Same pipeline on a laptop, a VPS, or any orchestrator — the consumer's
workspace mounts at /work/<repo> and scripts/dev-link.sh rewrites the
`[patch]` block to the sibling checkouts actually mounted under /work;
remaining git deps resolve over https (float on `main`).

Private deps: if the repo's git deps are private → pass a GitHub token as
`--token` (mounted secret; the insteadOf rewrite runs inside the exec,
never in an image layer).
"""

import asyncio
from typing import Annotated, Optional

import dagger
from dagger import DefaultPath, Ignore, dag, function, object_type

# Consumer repo root as the default source, minus directories that must
# never be uploaded to the engine (target/ alone can be ~8.5 GB).
Source = Annotated[
    dagger.Directory,
    DefaultPath("."),
    Ignore(["target", ".git", "coverage", "node_modules",
            "dagger/.venv", "dagger/sdk"]),
]

CARGO_ENV = {
    "CARGO_INCREMENTAL": "0",
    "CARGO_TERM_COLOR": "always",
    "CARGO_PROFILE_DEV_DEBUG": "0",
    "CARGO_PROFILE_TEST_DEBUG": "0",
    # Cargo must shell out to git so the insteadOf credential rewrite and
    # ssh/config auth both apply to private git deps.
    "CARGO_NET_GIT_FETCH_WITH_CLI": "true",
}

# Rewrites https://github.com/* fetches through the mounted token. Runs
# inside each exec (not a build layer) so the token never persists in an
# image layer or the cache.
_AUTH_GIT_CONFIG = (
    'git config --global url."https://x-access-token:$(cat /run/secrets/bupha-token)'
    '@github.com/".insteadOf "https://github.com/" && '
)

# Appends a `[patch."<repo-git-url>"]` table pointing the repo's own
# crates at their local paths — unless one already exists. Needed because
# transitive git deps can reference the repo's own crates: without the
# self-patch cargo sees two sources for the same package (ambiguous -p
# spec, and stale-`main` code in the dep graph instead of the checkout
# under test). The patch URL comes from `patch_host` when given, else the
# repo's own `repository` field in Cargo.toml — any org, zero config.
_SELF_PATCH_PY = '''python3 - <<'PYEOF'
import re, pathlib, tomllib
repo_dir = pathlib.Path("REPO_DIR")
toml = repo_dir / "Cargo.toml"
patch_host = "__PATCH_HOST__"   # substituted by the module; "" = derive
if patch_host and not patch_host.startswith("__"):
    key = f'[patch."{patch_host}/REPO_NAME"]'
else:
    # no host passed — derive the patch URL from the repo's own
    # Cargo.toml `repository` field (any org, zero config)
    meta = tomllib.loads(toml.read_text())
    url = (meta.get("workspace", {}).get("package", {})
               or meta.get("package", {})).get("repository", "")
    if not url:
        raise SystemExit("self-patch: no patch_host and no repository in Cargo.toml")
    key = '[patch."%s"]' % url.removesuffix(".git")
lines = toml.read_text().splitlines(keepends=True)
out, skipping = [], False
for l in lines:
    if l.startswith(key):
        skipping = True
        continue
    if skipping and l.lstrip().startswith("["):
        skipping = False
    if not skipping:
        out.append(l)
# normalize tail whitespace so re-runs are byte-identical
toml.write_text("".join(out).rstrip("\\n") + "\\n\\n")
if True:
    rows = []
    for g in ("crates", "modules"):
        for t in sorted(repo_dir.glob(f"{g}/*/Cargo.toml")):
            m = re.search(r'^name = "([^"]+)"', t.read_text(), re.M)
            if m:
                rows.append((m.group(1), t.parent))
    with toml.open("a") as f:
        f.write(key + "\\n")
        for n, p in rows:
            rel = p.relative_to(repo_dir.parent)
            f.write(f'{n} = {{ path = "../{rel}" }}\\n')
PYEOF'''


@object_type
class Bupha:
    # -- container plumbing ------------------------------------------------

    def _base(self) -> dagger.Container:
        """Toolchain image: stable rust + nextest + llvm-cov + sccache."""
        return (
            dag.container()
            .from_("rust:slim")
            .with_exec(["apt-get", "update"])
            .with_exec([
                "apt-get", "install", "-y", "--no-install-recommends",
                "git", "curl", "ca-certificates", "python3",
                "build-essential", "pkg-config", "libssl-dev", "clang", "lld",
            ])
            .with_exec(["sh", "-c",
                        "curl -LsSf https://get.nexte.st/latest/linux"
                        " | tar zxf - -C /usr/local/cargo/bin"])
            .with_exec(["rustup", "component", "add", "llvm-tools-preview"])
            .with_exec(["sh", "-c",
                        "curl -LsSf https://github.com/taiki-e/cargo-llvm-cov"
                        "/releases/latest/download/"
                        "cargo-llvm-cov-x86_64-unknown-linux-musl.tar.gz"
                        " | tar xzf - -C /usr/local/cargo/bin"])
            .with_exec(["sh", "-c",
                        "TAG=$(curl -s https://api.github.com/repos/mozilla"
                        "/sccache/releases/latest | grep '\"tag_name\"'"
                        " | cut -d'\"' -f4)"
                        " && curl -LsSf https://github.com/mozilla/sccache"
                        "/releases/download/$TAG"
                        "/sccache-$TAG-x86_64-unknown-linux-musl.tar.gz"
                        " | tar xzf - --strip-components=1"
                        " -C /usr/local/cargo/bin"
                        " sccache-$TAG-x86_64-unknown-linux-musl/sccache"])
        )

    def _workspace(
        self,
        source: dagger.Directory,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        siblings: Optional[dict] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> dagger.Container:
        """Base image + source at /work/<repo> + cargo caches + dev-link.

        dev-link.sh rewrites Cargo.toml's patch block to only the sibling
        checkouts present under /work; all other git deps then
        resolve from `main` over https (authenticated when token is set).

        sccache_conf: optional env file (KEY=VAL per line) with SCCACHE_*
        remote-backend settings (S3/Redis). Without it sccache uses the
        local cache volume.
        """
        ctr = self._base().with_mounted_directory(f"/work/{repo}", source)
        for name, directory in (siblings or {}).items():
            ctr = ctr.with_mounted_directory(f"/work/{name}", directory)
        ctr = (
            ctr.with_workdir(f"/work/{repo}")
            .with_env_variable("RUSTC_WRAPPER", "sccache")
            .with_env_variable("SCCACHE_DIR", "/sccache")
        )
        for key, value in CARGO_ENV.items():
            ctr = ctr.with_env_variable(key, value)
        if token is not None:
            ctr = ctr.with_mounted_secret("/run/secrets/bupha-token", token)
        if sccache_conf is not None:
            ctr = ctr.with_mounted_secret(
                "/run/secrets/sccache-env", sccache_conf)
        ctr = (
            ctr
            .with_mounted_cache("/usr/local/cargo/registry",
                                dag.cache_volume("bupha-cargo-registry"))
            .with_mounted_cache("/usr/local/cargo/git",
                                dag.cache_volume("bupha-cargo-git"))
            .with_mounted_cache(f"/work/{repo}/target",
                                dag.cache_volume(f"bupha-{repo}-target"))
            .with_mounted_cache("/sccache",
                                dag.cache_volume("bupha-sccache"))
        )
        # dev-link each mounted repo (main repo's script preferred, then
        # the sibling's own, else strip the block), then self-patch so
        # transitive git refs to a mounted repo resolve to its checkout.
        repos = [repo, *(siblings or {})]
        for name in repos:
            ctr = self._link_repo(ctr, repo, name, patch_host)
        return ctr

    def _link_repo(
        self, ctr: dagger.Container, main_repo: str, name: str,
        patch_host: str = "",
    ) -> dagger.Container:
        """dev-link + self-patch one mounted repo under /work/<name>."""
        ctr = ctr.with_exec(["bash", "-c",
                             f"if [ -f /work/{main_repo}/scripts/dev-link.sh ]; then"
                             f" bash /work/{main_repo}/scripts/dev-link.sh /work/{name};"
                             f" elif [ -f /work/{name}/scripts/dev-link.sh ]; then"
                             f" bash /work/{name}/scripts/dev-link.sh /work/{name};"
                             " else sed -i '/# dev-link: begin/,/# dev-link: end/d'"
                             f" /work/{name}/Cargo.toml 2>/dev/null || true; fi"])
        script = (_SELF_PATCH_PY
                  .replace("REPO_DIR", f"/work/{name}")
                  .replace("REPO_NAME", name)
                  .replace("__PATCH_HOST__", patch_host))
        return ctr.with_exec(["bash", "-c", script])

    # -- composable primitives (for consumer modules) ------------------------

    @function
    def workspace(
        self,
        source: Source,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> dagger.Container:
        """Workspace container: source at /work/<repo>, caches, dev-link, self-patch."""
        return self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)

    @function
    def with_sibling(
        self,
        ctr: dagger.Container,
        name: str,
        source: dagger.Directory,
        main_repo: str = "repo",
        patch_host: str = "",
    ) -> dagger.Container:
        """Mount a sibling checkout at /work/<name>, link it, cache its target/."""
        return self._link_repo(
            ctr.with_mounted_directory(f"/work/{name}", source)
               .with_mounted_cache(f"/work/{name}/target",
                                   dag.cache_volume(f"bupha-{name}-target")),
            main_repo, name, patch_host)

    @function
    def run_script(
        self,
        ctr: dagger.Container,
        script: str,
        authed: bool = False,
        sccache_env: bool = False,
    ) -> dagger.Container:
        """Run a shell script in a workspace container (auth + sccache env)."""
        return self._exec(ctr, script, authed, sccache_env)

    def _exec(
        self,
        ctr: dagger.Container,
        script: str,
        authed: bool,
        sccache_env: bool = False,
        patch_host: str = "",
    ) -> dagger.Container:
        """Run a shell script, prefixed with auth + sccache-env setup."""
        if authed:
            script = _AUTH_GIT_CONFIG + script
        if sccache_env:
            script = (
                "set -a && . /run/secrets/sccache-env && set +a && " + script
            )
        return ctr.with_exec(["bash", "-c", script])

    # -- pipeline functions --------------------------------------------------

    @function
    async def doctor(
        self,
        source: Source,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> str:
        """Smoke-test the workspace container: toolchain versions + dev-link.

        Cheap end-to-end check of everything except compiling Rust —
        image build, source mount, dev-link patch rewrite, git auth.
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        return await self._exec(
            ws,
            "rustc --version && cargo nextest --version"
            " && cargo llvm-cov --version && sccache --version"
            " && { grep -c 'patch\\.' Cargo.toml || true; }",
            token is not None, sccache_conf is not None,
        ).stdout()

    @function
    async def test_shard(
        self,
        source: Source,
        shard: int = 1,
        shards: int = 1,
        jobs: int = 2,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> str:
        """Run one nextest partition (count:shard/shards) over the workspace.

        -j is deliberately low: heavy suites are memory-bound — scale out
        via `shards`, not up via `jobs`.
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        return await self._exec(
            ws,
            "cargo nextest run --workspace "
            f"-j {jobs} --partition count:{shard}/{shards}",
            token is not None, sccache_conf is not None,
        ).stdout()

    @function
    async def doctests(
        self,
        source: Source,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> str:
        """Run workspace doctests (nextest does not cover them)."""
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        return await self._exec(
            ws, "cargo test --doc --workspace",
            token is not None, sccache_conf is not None,
        ).stdout()

    @function
    async def test(
        self,
        source: Source,
        shards: int = 1,
        jobs: int = 2,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> str:
        """Run the full test gate: `shards` parallel nextest partitions + doctests."""
        # Warm the shared cargo git db serially before fan-out: two
        # concurrent first-time fetches race on the same cache volume
        # ("failed to remove directory ... objects" errors).
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        await self._exec(
            ws, "cargo fetch --locked",
            token is not None, sccache_conf is not None,
        ).sync()
        tasks = [
            self.test_shard(source, i + 1, shards, jobs, repo,
                            token, sccache_conf, patch_host)
            for i in range(shards)
        ]
        tasks.append(
            self.doctests(source, repo, token, sccache_conf, patch_host))
        results = await asyncio.gather(*tasks, return_exceptions=True)
        failures = [r for r in results if isinstance(r, BaseException)]
        if failures:
            raise RuntimeError(
                f"{len(failures)} of {len(results)} test stages failed:\n"
                + "\n".join(str(f) for f in failures)
            )
        return f"{shards} shard(s) + doctests passed"

    @function
    async def coverage_report(
        self,
        source: Source,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> dagger.Directory:
        """Run scripts/coverage.sh and return the coverage/ directory.

        Requires the repo to carry scripts/coverage.sh (platform's
        convention). Export: dagger call ... coverage-report export
        --path=coverage
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        await self._exec(
            ws,
            "./scripts/coverage.sh --summary"
            " && cargo llvm-cov --workspace --lcov"
            " --output-path coverage/lcov.info",
            token is not None, sccache_conf is not None,
        ).sync()
        return ws.directory(f"/work/{repo}/coverage")

    @function
    async def coverage_gate(
        self,
        source: Source,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> str:
        """Blocking gate: scripts/coverage.sh --tooling-gate."""
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        await self._exec(
            ws, "./scripts/coverage.sh --tooling-gate",
            token is not None, sccache_conf is not None,
        ).sync()
        return "tooling coverage gate passed"

    @function
    async def run(
        self,
        source: Source,
        script: str,
        repo: str = "repo",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
        patch_host: str = "",
    ) -> str:
        """Run an arbitrary shell script inside the workspace container.

        Escape hatch for repo-specific gates without writing module code.
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf,
                           patch_host=patch_host)
        return await self._exec(
            ws, script, token is not None, sccache_conf is not None
        ).stdout()
