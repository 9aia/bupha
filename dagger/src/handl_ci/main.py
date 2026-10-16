"""Shared CI primitives for handl-lang repos — one definition, runs anywhere.

Install into a consumer repo:

    dagger install github.com/handl-lang/ci@main
    # then:  dag.handl_ci().test(source)  or  dagger call -m ... test

Same pipeline on a laptop, a VPS, or any orchestrator — the consumer's
workspace mounts at /work/<repo> and scripts/dev-link.sh rewrites the
`[patch]` block to the sibling checkouts actually mounted under /work;
remaining handl-lang git deps resolve over https (float on `main`).

Private deps: every handl-lang repo is private → pass a GitHub token as
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
    'git config --global url."https://x-access-token:$(cat /run/secrets/hl-token)'
    '@github.com/".insteadOf "https://github.com/" && '
)


@object_type
class HandlCi:
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
                "build-essential", "pkg-config", "libssl-dev",
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        siblings: Optional[dict] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> dagger.Container:
        """Base image + source at /work/<repo> + cargo caches + dev-link.

        dev-link.sh rewrites Cargo.toml's patch block to only the sibling
        checkouts present under /work; all other handl-lang git deps then
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
            ctr = ctr.with_mounted_secret("/run/secrets/hl-token", token)
        if sccache_conf is not None:
            ctr = ctr.with_mounted_secret(
                "/run/secrets/sccache-env", sccache_conf)
        return (
            ctr
            .with_mounted_cache("/usr/local/cargo/registry",
                                dag.cache_volume("hl-cargo-registry"))
            .with_mounted_cache("/usr/local/cargo/git",
                                dag.cache_volume("hl-cargo-git"))
            .with_mounted_cache(f"/work/{repo}/target",
                                dag.cache_volume(f"hl-{repo}-target"))
            .with_mounted_cache("/sccache",
                                dag.cache_volume("hl-sccache"))
            .with_exec(["bash", "-c",
                        "if [ -f scripts/dev-link.sh ]; then"
                        f" bash scripts/dev-link.sh /work/{repo};"
                        " else sed -i '/# dev-link: begin/,/# dev-link: end/d'"
                        " Cargo.toml; fi"])
        )

    def _exec(
        self,
        ctr: dagger.Container,
        script: str,
        authed: bool,
        sccache_env: bool = False,
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Smoke-test the workspace container: toolchain versions + dev-link.

        Cheap end-to-end check of everything except compiling Rust —
        image build, source mount, dev-link patch rewrite, git auth.
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf)
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Run one nextest partition (count:shard/shards) over the workspace.

        -j is deliberately low: heavy suites are memory-bound — scale out
        via `shards`, not up via `jobs`.
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf)
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Run workspace doctests (nextest does not cover them)."""
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf)
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Run the full test gate: `shards` parallel nextest partitions + doctests."""
        tasks = [
            self.test_shard(source, i + 1, shards, jobs, repo,
                            token, sccache_conf)
            for i in range(shards)
        ]
        tasks.append(self.doctests(source, repo, token, sccache_conf))
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> dagger.Directory:
        """Run scripts/coverage.sh and return the coverage/ directory.

        Requires the repo to carry scripts/coverage.sh (platform's
        convention). Export: dagger call ... coverage-report export
        --path=coverage
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf)
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Blocking gate: scripts/coverage.sh --tooling-gate."""
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf)
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
        repo: str = "platform",
        token: Optional[dagger.Secret] = None,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Run an arbitrary shell script inside the workspace container.

        Escape hatch for repo-specific gates without writing module code.
        """
        ws = self._workspace(source, repo, token, sccache_conf=sccache_conf)
        return await self._exec(
            ws, script, token is not None, sccache_conf is not None
        ).stdout()
