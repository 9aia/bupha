"""Regression tests for the embedded _SELF_PATCH_PY script.

The script ships inside a bash heredoc in main.py; tests pull the
decoded string out of the module AST (so `\\n` escapes resolve the way
the runtime sees them), apply the same placeholder substitution
`_link_repo` does, and run it against fixture Cargo.tomls in tmp dirs.

Run: pytest modules/dagger/tests/
"""
import ast
import builtins
import pathlib
import re
import subprocess

MAIN = (pathlib.Path(__file__).parent.parent
        / "dagger/src/bupha/main.py")


def module_source() -> str:
    return MAIN.read_text()


def extract() -> str:
    """The decoded _SELF_PATCH_PY body (what bash actually receives)."""
    tree = ast.parse(module_source())
    for n in tree.body:
        if (isinstance(n, ast.Assign)
                and getattr(n.targets[0], "id", "") == "_SELF_PATCH_PY"):
            body = n.value.value
            m = re.search(r"<<'PYEOF'\n(.*?)PYEOF", body, re.S)
            assert m, "PYEOF heredoc not found in _SELF_PATCH_PY"
            return m.group(1)
    raise AssertionError("_SELF_PATCH_PY not found")


def run(repo: pathlib.Path, name: str = "fixture",
        patch_host: str = "") -> None:
    script = (extract()
              .replace("REPO_DIR", str(repo))
              .replace("REPO_NAME", name)
              .replace("__PATCH_HOST__", patch_host))
    r = subprocess.run(["python3", "-c", script],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def run_raw(repo: pathlib.Path, name: str = "fixture") -> subprocess.CompletedProcess:
    script = (extract()
              .replace("REPO_DIR", str(repo))
              .replace("REPO_NAME", name)
              .replace("__PATCH_HOST__", ""))
    return subprocess.run(["python3", "-c", script],
                          capture_output=True, text=True)


def make_repo(tmp_path: pathlib.Path) -> pathlib.Path:
    repo = tmp_path / "fixture"
    (repo / "crates/foo").mkdir(parents=True)
    (repo / "crates/bar").mkdir(parents=True)
    (repo / "modules/baz").mkdir(parents=True)
    for d, n in [(repo / "crates/foo", "foo"),
                 (repo / "crates/bar", "bar"),
                 (repo / "modules/baz", "baz")]:
        (d / "Cargo.toml").write_text(
            f'[package]\nname = "{n}"\nversion = "0.0.0"\n')
    return repo


def test_appends_patch_block_from_repository_field(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "Cargo.toml").write_text(
        '[workspace.package]\n'
        'repository = "https://github.com/o/repo-x"\n'
        'members = []\n')
    run(repo)
    text = (repo / "Cargo.toml").read_text()
    assert '[patch."https://github.com/o/repo-x"]' in text
    # rows point at ../<repo>/<crate> — resolvable from /work/<repo>
    assert 'foo = { path = "../fixture/crates/foo" }' in text
    assert 'bar = { path = "../fixture/crates/bar" }' in text
    assert 'baz = { path = "../fixture/modules/baz" }' in text


def test_patch_host_overrides_repository(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "Cargo.toml").write_text(
        '[workspace.package]\n'
        'repository = "https://github.com/o/repo-x"\nmembers = []\n')
    run(repo, patch_host="https://github.com/other-org")
    text = (repo / "Cargo.toml").read_text()
    # host + repo name — NOT the repository url
    assert '[patch."https://github.com/other-org/fixture"]' in text


def test_idempotent_rerun(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "Cargo.toml").write_text(
        '[workspace.package]\n'
        'repository = "https://github.com/o/repo-x"\nmembers = []\n')
    run(repo)
    once = (repo / "Cargo.toml").read_text()
    run(repo)
    assert (repo / "Cargo.toml").read_text() == once
    assert once.count('[patch."') == 1


def test_strips_stale_rows_and_inner_comments(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "Cargo.toml").write_text(
        '# keep this comment\n'
        '[workspace.package]\n'
        'repository = "https://github.com/o/repo-x"\nmembers = []\n\n'
        '[patch."https://github.com/o/repo-x"]\n'
        'stale = { path = "../old" }\n'
        '# a comment inside the old block\n'
        'alsostale = { path = "../old2" }\n\n'
        '[dependencies]\nfoo = "1"\n')
    run(repo)
    text = (repo / "Cargo.toml").read_text()
    assert "stale" not in text and "alsostale" not in text
    assert "# keep this comment" in text
    assert "[dependencies]" in text          # tables after the block survive
    assert 'foo = { path = "../fixture/crates/foo" }' in text


def test_no_repository_no_patch_host_errors(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "Cargo.toml").write_text('[workspace]\nmembers = []\n')
    r = run_raw(repo)
    assert r.returncode != 0
    assert "no patch_host" in r.stderr


def test_no_public_fn_forwards_undeclared_names():
    """The patch_host regression class: every Load name in a function
    must be a param, a local binding, a module global, or a builtin."""
    tree = ast.parse(module_source())
    globals_ = {n.targets[0].id for n in tree.body
                if isinstance(n, ast.Assign)
                and isinstance(n.targets[0], ast.Name)}
    allowed = globals_ | set(dir(builtins)) | {
        "dagger", "dag", "function", "object_type", "asyncio",
        "Optional", "Annotated", "DefaultPath", "Ignore", "self"}
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        bound = ({a.arg for a in node.args.args + node.args.kwonlyargs}
                 | {n.id for n in ast.walk(node)
                    if isinstance(n, ast.Name)
                    and isinstance(n.ctx, ast.Store)})
        # names bound by `except as`, `with as`, imports inside fns
        for sub in ast.walk(node):
            for attr in ("name", "asname", "optional_vars"):
                v = getattr(sub, attr, None)
                if isinstance(v, ast.Name):
                    bound.add(v.id)
                elif isinstance(v, ast.expr) and hasattr(v, "id"):
                    bound.add(v.id)
                elif isinstance(v, str):
                    bound.add(v)
        used = {n.id for n in ast.walk(node)
                if isinstance(n, ast.Name)
                and isinstance(n.ctx, ast.Load)}
        missing = used - bound - allowed
        assert not missing, f"{node.name} uses undeclared {missing}"
