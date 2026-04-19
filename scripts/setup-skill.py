#!/usr/bin/env python3
"""Install the reCamera Intellisense skill into a compatible skills directory.

Features:
- interactive or non-interactive install
- local-checkout installs without network access
- standalone installs from GitHub release source archives
- safe staged replacement with rollback on failure
- destination discovery for workspace, Claude Code, and Claw-style roots

Examples:
    python3 setup-skill.py
    python3 setup-skill.py install -y
    python3 setup-skill.py check
    python3 setup-skill.py uninstall -y
    python3 setup-skill.py list-destinations
    curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-skill.py \
      | python3 - install --yes
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import sys
import tarfile
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Iterator, Sequence
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

REPO = "iChizer0/reCamera-Intellisense"
SKILL_NAME = "recamera-intellisense"
DEFAULT_BRANCH = "main"
API_TIMEOUT = 30
DOWNLOAD_TIMEOUT = 180
CHUNK_SIZE = 64 * 1024
CONFIG_DIR_NAMES = {".claude", ".openclaw", ".claw", "claude", "openclaw", "claw"}
WORKSPACE_MARKERS = (".git", "pyproject.toml", "package.json", "Cargo.toml", "go.mod")
IGNORE_NAMES = {".git", "__pycache__", ".DS_Store"}
SKILL_REL = PurePosixPath("skills") / SKILL_NAME
PKG_REL = PurePosixPath("recamera-intellisense-sdk") / "recamera_intellisense"


def _use_colour() -> bool:
    if os.environ.get("NO_COLOR"):
        return False
    if os.environ.get("FORCE_COLOR"):
        return True
    return sys.stdout.isatty()


class Ui:
    colour = _use_colour()
    auto_yes = False

    @classmethod
    def _wrap(cls, code: str, text: str) -> str:
        return f"\033[{code}m{text}\033[0m" if cls.colour else text

    @classmethod
    def bold(cls, text: str) -> str:
        return cls._wrap("1", text)

    @classmethod
    def dim(cls, text: str) -> str:
        return cls._wrap("2", text)

    @classmethod
    def green(cls, text: str) -> str:
        return cls._wrap("32", text)

    @classmethod
    def yellow(cls, text: str) -> str:
        return cls._wrap("33", text)

    @classmethod
    def red(cls, text: str) -> str:
        return cls._wrap("1;31", text)

    @classmethod
    def cyan(cls, text: str) -> str:
        return cls._wrap("36", text)

    @classmethod
    def info(cls, message: str) -> None:
        print(f" {cls.green('✓')} {message}")

    @classmethod
    def step(cls, message: str) -> None:
        print(f" {cls.cyan('→')} {message}")

    @classmethod
    def warn(cls, message: str) -> None:
        print(f" {cls.yellow('!')} {message}")

    @classmethod
    def error(cls, message: str) -> None:
        print(f" {cls.red('✗')} {message}", file=sys.stderr)

    @classmethod
    def hint(cls, message: str) -> None:
        print(f"   {cls.dim('hint:')} {message}", file=sys.stderr)

    @classmethod
    def rule(cls, title: str) -> None:
        bar = "─" * max(60, len(title) + 4)
        padding = " " * ((len(bar) - len(title) - 2) // 2)
        print()
        print(cls.bold(bar))
        print(cls.bold(f"{padding}{title}"))
        print(cls.bold(bar))

    @classmethod
    def ask_yes_no(cls, prompt: str, default: bool = True) -> bool:
        if cls.auto_yes or not sys.stdin.isatty():
            return default
        hint = "Y/n" if default else "y/N"
        try:
            raw = input(f" ? {prompt} [{hint}]: ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            print()
            return default
        if not raw:
            return default
        return raw in {"y", "yes"}

    @classmethod
    def ask_choice(
        cls,
        prompt: str,
        options: Sequence[str],
        default_index: int = 0,
    ) -> int:
        if cls.auto_yes or not sys.stdin.isatty() or len(options) == 1:
            return default_index
        while True:
            print(f" ? {prompt}")
            for index, option in enumerate(options, start=1):
                marker = cls.cyan("→") if index - 1 == default_index else " "
                print(f"   {marker} {index}) {option}")
            try:
                raw = input(f"   Select [default {default_index + 1}]: ").strip()
            except (EOFError, KeyboardInterrupt):
                print()
                return default_index
            if not raw:
                return default_index
            if raw.isdigit():
                chosen = int(raw) - 1
                if 0 <= chosen < len(options):
                    return chosen
            cls.warn(f"Please enter a number between 1 and {len(options)}.")


class SetupError(Exception):
    def __init__(self, message: str, hint: str | None = None, exit_code: int = 1):
        super().__init__(message)
        self.hint = hint
        self.exit_code = exit_code


@dataclass(frozen=True)
class Paths:
    anchor: Path
    skill_dir: Path

    @property
    def skills_dir(self) -> Path:
        return self.skill_dir.parent

    @property
    def package_dir(self) -> Path:
        return self.skill_dir / "scripts" / "recamera_intellisense"


@dataclass(frozen=True)
class Candidate:
    label: str
    reason: str
    recommended: bool
    paths: Paths
    state: str
    problem: str | None


@dataclass(frozen=True)
class LocalSource:
    repo_root: Path
    skill_dir: Path
    package_dir: Path


@dataclass(frozen=True)
class BundleSource:
    label: str
    bundle_dir: Path


def _script_file() -> Path | None:
    raw = globals().get("__file__")
    if not raw or str(raw).startswith("<"):
        return None
    return Path(raw).resolve()


def _remove_path(path: Path) -> None:
    if not path.exists() and not path.is_symlink():
        return
    if path.is_symlink() or path.is_file():
        path.unlink()
    else:
        shutil.rmtree(path)


def _next_backup_path(path: Path) -> Path:
    candidate = path.with_name(path.name + ".bak")
    suffix = 2
    while candidate.exists() or candidate.is_symlink():
        candidate = path.with_name(f"{path.name}.bak{suffix}")
        suffix += 1
    return candidate


def _ignore_entries(_src: str, names: list[str]) -> set[str]:
    return {name for name in names if name in IGNORE_NAMES or name.endswith(".pyc")}


def _format_mb(size: int) -> str:
    return f"{size / (1024 * 1024):.1f} MB"


def _read_skill_name(skill_md: Path) -> str | None:
    try:
        lines = skill_md.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    if not lines or lines[0].strip() != "---":
        return None
    for line in lines[1:]:
        if line.strip() == "---":
            break
        if line.startswith("name:"):
            return line.split(":", 1)[1].strip()
    return None


def inspect_skill(skill_dir: Path) -> tuple[bool, str | None]:
    skill_md = skill_dir / "SKILL.md"
    package_dir = skill_dir / "scripts" / "recamera_intellisense"
    if not skill_md.is_file():
        return False, f"Missing {skill_md}"
    if not (package_dir / "__main__.py").is_file() or not (package_dir / "__init__.py").is_file():
        return False, f"Missing bundled package under {package_dir}"
    name = _read_skill_name(skill_md)
    if name and name != SKILL_NAME:
        return False, f"Skill manifest name is {name!r}, expected {SKILL_NAME!r}"
    return True, None


def _copy_bundle(skill_src: Path, pkg_src: Path, bundle_dir: Path) -> None:
    if not (skill_src / "SKILL.md").is_file():
        raise SetupError(f"Skill source is missing SKILL.md: {skill_src}")
    if not (pkg_src / "__main__.py").is_file():
        raise SetupError(f"Python package source is incomplete: {pkg_src}")
    if bundle_dir.exists() or bundle_dir.is_symlink():
        _remove_path(bundle_dir)
    shutil.copytree(skill_src, bundle_dir, symlinks=False, ignore=_ignore_entries)
    target_pkg = bundle_dir / "scripts" / "recamera_intellisense"
    if target_pkg.exists() or target_pkg.is_symlink():
        _remove_path(target_pkg)
    target_pkg.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(pkg_src, target_pkg, symlinks=False, ignore=_ignore_entries)
    ok, problem = inspect_skill(bundle_dir)
    if not ok:
        raise SetupError(problem or "Assembled bundle failed validation.")


def detect_local_source() -> LocalSource | None:
    script = _script_file()
    if script is None:
        return None
    repo_root = script.parent.parent
    skill_dir = repo_root / "skills" / SKILL_NAME
    if not (skill_dir / "SKILL.md").is_file():
        skill_dir = repo_root / "scripts" / "skills" / SKILL_NAME
    package_dir = repo_root / "recamera-intellisense-sdk" / "recamera_intellisense"
    if not (skill_dir / "SKILL.md").is_file() or not (package_dir / "__main__.py").is_file():
        return None
    return LocalSource(repo_root=repo_root, skill_dir=skill_dir, package_dir=package_dir)


def _github_get(url: str, timeout: int) -> dict:
    req = Request(url, headers={"Accept": "application/vnd.github+json"})
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urlopen(req, timeout=timeout) as response:
            return json.loads(response.read())
    except HTTPError as exc:
        if exc.code == 404:
            raise SetupError(
                "GitHub release not found.",
                hint="The tag passed via --version does not exist, or the repo has no releases yet.",
            ) from None
        if exc.code in (401, 403):
            raise SetupError(
                f"GitHub API rejected the request (HTTP {exc.code}).",
                hint="If you are rate-limited, set GITHUB_TOKEN in the environment.",
            ) from None
        raise SetupError(f"GitHub API error: HTTP {exc.code}") from None
    except URLError as exc:
        raise SetupError(
            f"Network error contacting GitHub: {exc.reason}",
            hint="Check your internet connection and any HTTP_PROXY settings.",
        ) from None


def fetch_release(tag: str | None) -> dict:
    if tag:
        Ui.step(f"Fetching release {Ui.bold(tag)} from GitHub…")
        return _github_get(f"https://api.github.com/repos/{REPO}/releases/tags/{tag}", API_TIMEOUT)
    Ui.step("Fetching latest release from GitHub…")
    return _github_get(f"https://api.github.com/repos/{REPO}/releases/latest", API_TIMEOUT)


def _codeload_tag_url(tag: str) -> str:
    return f"https://codeload.github.com/{REPO}/tar.gz/refs/tags/{tag}"


def _codeload_branch_url(branch: str) -> str:
    return f"https://codeload.github.com/{REPO}/tar.gz/refs/heads/{branch}"


def _download(url: str, dest: Path) -> None:
    req = Request(url)
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urlopen(req, timeout=DOWNLOAD_TIMEOUT) as response:
            total_hdr = response.headers.get("Content-Length")
            total = int(total_hdr) if total_hdr else 0
            received = 0
            last_pct = -1
            with open(dest, "wb") as handle:
                while True:
                    chunk = response.read(CHUNK_SIZE)
                    if not chunk:
                        break
                    handle.write(chunk)
                    received += len(chunk)
                    if sys.stdout.isatty() and total:
                        pct = received * 100 // total
                        if pct != last_pct:
                            last_pct = pct
                            print(
                                f"\r   Downloading… {_format_mb(received)} / {_format_mb(total)} ({pct}%)",
                                end="",
                                flush=True,
                            )
            if sys.stdout.isatty() and total:
                print()
    except (HTTPError, URLError) as exc:
        raise SetupError(f"Download failed: {exc}") from None


def _strip_archive_root(member_name: str) -> PurePosixPath | None:
    pure = PurePosixPath(member_name)
    if len(pure.parts) < 2:
        return None
    rel = PurePosixPath(*pure.parts[1:])
    if rel.is_absolute() or any(part in {"", ".", ".."} for part in rel.parts):
        return None
    return rel if rel.parts else None


def _wanted(rel: PurePosixPath) -> bool:
    text = rel.as_posix()
    skill_root = SKILL_REL.as_posix()
    pkg_root = PKG_REL.as_posix()
    return text == skill_root or text.startswith(skill_root + "/") or text == pkg_root or text.startswith(pkg_root + "/")


def _extract_source_tree(archive: Path, dest_root: Path) -> tuple[Path, Path]:
    extracted = False
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            rel = _strip_archive_root(member.name)
            if rel is None or not _wanted(rel) or not member.isfile():
                continue
            target = dest_root / Path(*rel.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            fileobj = tar.extractfile(member)
            if fileobj is None:
                continue
            with open(target, "wb") as handle:
                shutil.copyfileobj(fileobj, handle)
            extracted = True
    if not extracted:
        raise SetupError("Source archive did not contain the expected skill files.")
    skill_src = dest_root / Path(*SKILL_REL.parts)
    pkg_src = dest_root / Path(*PKG_REL.parts)
    if not (skill_src / "SKILL.md").is_file() or not (pkg_src / "__main__.py").is_file():
        raise SetupError("Downloaded archive was missing required skill files.")
    return skill_src, pkg_src


def _prepare_release_bundle(temp_root: Path, version: str | None) -> BundleSource:
    if version:
        release = fetch_release(version)
        tag = release.get("tag_name", version)
        label = f"GitHub release {tag}"
        archive_url = _codeload_tag_url(tag)
    else:
        try:
            release = fetch_release(None)
        except SetupError as exc:
            Ui.warn("Could not resolve the latest release; falling back to the main branch source archive.")
            if exc.hint:
                Ui.hint(exc.hint)
            label = f"GitHub branch {DEFAULT_BRANCH}"
            archive_url = _codeload_branch_url(DEFAULT_BRANCH)
        else:
            tag = release.get("tag_name", "latest")
            label = f"GitHub release {tag}"
            archive_url = _codeload_tag_url(tag)
    archive = temp_root / "source.tar.gz"
    Ui.step(f"Downloading {Ui.bold(label)} source archive…")
    _download(archive_url, archive)
    extracted_root = temp_root / "extracted"
    skill_src, pkg_src = _extract_source_tree(archive, extracted_root)
    bundle_dir = temp_root / SKILL_NAME
    _copy_bundle(skill_src, pkg_src, bundle_dir)
    return BundleSource(label=label, bundle_dir=bundle_dir)


@contextmanager
def source_bundle(version: str | None, force_download: bool) -> Iterator[BundleSource]:
    local = detect_local_source()
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_root = Path(temp_dir)
        if local is not None and not version and not force_download:
            bundle_dir = temp_root / SKILL_NAME
            Ui.step("Using local checkout sources…")
            _copy_bundle(local.skill_dir, local.package_dir, bundle_dir)
            yield BundleSource(label=f"local checkout ({local.repo_root})", bundle_dir=bundle_dir)
        else:
            yield _prepare_release_bundle(temp_root, version)


def _looks_like_config_dir(path: Path) -> bool:
    name = path.name.lower()
    return name in CONFIG_DIR_NAMES or (name.startswith(".") and "claw" in name)


def normalise_anchor(raw: str | Path) -> Path:
    path = Path(raw).expanduser()
    if path.name == "skills":
        return path.parent
    if _looks_like_config_dir(path):
        return path
    return path / ".claude"


def explicit_paths(raw: str | Path) -> Paths:
    path = Path(raw).expanduser()
    if path.name == SKILL_NAME:
        anchor = path.parent.parent if path.parent.name == "skills" else path.parent
        return Paths(
            anchor=anchor.expanduser().resolve(strict=False),
            skill_dir=path.expanduser().resolve(strict=False),
        )
    return _paths(normalise_anchor(path))


def _paths(anchor: Path) -> Paths:
    anchor = anchor.expanduser().resolve(strict=False)
    return Paths(anchor=anchor, skill_dir=anchor / "skills" / SKILL_NAME)


def _candidate(anchor: Path, label: str, reason: str, recommended: bool) -> Candidate:
    return candidate_from_paths(_paths(anchor), label, reason, recommended)


def candidate_from_paths(paths: Paths, label: str, reason: str, recommended: bool) -> Candidate:
    installed, problem = inspect_skill(paths.skill_dir)
    if installed:
        state = "installed"
    elif paths.skill_dir.exists() or paths.skill_dir.is_symlink():
        state = "incomplete"
    elif paths.anchor.exists():
        state = "available"
    else:
        state = "new"
    return Candidate(label=label, reason=reason, recommended=recommended, paths=paths, state=state, problem=problem)


def detect_workspace_root(start: Path) -> Path:
    start = start.resolve()
    for candidate in (start, *start.parents):
        if (candidate / ".git").exists():
            return candidate
        if any((candidate / marker).exists() for marker in WORKSPACE_MARKERS[1:]):
            return candidate
    return start


def discover_destinations(cwd: Path) -> list[Candidate]:
    workspace_root = detect_workspace_root(cwd)
    home = Path.home()
    xdg_home = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")).expanduser()
    raw_entries: list[tuple[Path, str, str]] = []
    seen: set[Path] = set()

    def add(anchor: Path, label: str, reason: str) -> None:
        resolved = anchor.expanduser().resolve(strict=False)
        if resolved in seen:
            return
        seen.add(resolved)
        raw_entries.append((resolved, label, reason))

    workspace_existing = [workspace_root / name for name in (".openclaw", ".claw", ".claude") if (workspace_root / name).exists()]
    if workspace_existing:
        for anchor in workspace_existing:
            add(anchor, f"workspace ({anchor.name})", "existing workspace skill root")
    else:
        add(workspace_root / ".claude", "workspace (.claude)", "recommended project-local skill root")
    add(workspace_root / ".openclaw", "workspace (.openclaw)", "project-local OpenClaw-style root")
    add(workspace_root / ".claw", "workspace (.claw)", "project-local Claw-style root")
    add(home / ".claude", "personal (~/.claude)", "personal Claude Code skill root")
    add(home / ".openclaw", "personal (~/.openclaw)", "personal OpenClaw-style root")
    add(home / ".claw", "personal (~/.claw)", "personal Claw-style root")
    add(xdg_home / "claude", f"XDG ({xdg_home.name}/claude)", "XDG-style Claude root")
    add(xdg_home / "openclaw", f"XDG ({xdg_home.name}/openclaw)", "XDG-style OpenClaw root")
    add(xdg_home / "claw", f"XDG ({xdg_home.name}/claw)", "XDG-style Claw root")
    return [_candidate(anchor, label, reason, index == 0) for index, (anchor, label, reason) in enumerate(raw_entries)]


def _format_state(state: str) -> str:
    if state == "installed":
        return Ui.green(state)
    if state == "incomplete":
        return Ui.yellow(state)
    return Ui.dim(state)


def _format_candidate(candidate: Candidate) -> str:
    parts = [candidate.label, str(candidate.paths.anchor), f"[{_format_state(candidate.state)}]"]
    if candidate.recommended:
        parts.append(Ui.green("recommended"))
    return " — ".join(parts)


def choose_destination(candidates: Sequence[Candidate]) -> Candidate:
    if not candidates:
        raise SetupError("No candidate skill destinations were found.")
    if Ui.auto_yes or not sys.stdin.isatty() or len(candidates) == 1:
        chosen = candidates[0]
        Ui.info(f"Destination: {Ui.cyan(str(chosen.paths.skill_dir))}")
        return chosen
    index = Ui.ask_choice("Choose where to install the skill:", [_format_candidate(c) for c in candidates])
    chosen = candidates[index]
    Ui.info(f"Selected: {Ui.cyan(str(chosen.paths.skill_dir))}")
    return chosen


def install_bundle(bundle_dir: Path, paths: Paths) -> bool:
    replaced = paths.skill_dir.exists() or paths.skill_dir.is_symlink()
    paths.anchor.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as temp_dir:
        stage_dir = Path(temp_dir) / SKILL_NAME
        shutil.copytree(bundle_dir, stage_dir)
        backup: Path | None = None
        if replaced:
            backup = _next_backup_path(paths.skill_dir)
            shutil.move(str(paths.skill_dir), str(backup))
        try:
            paths.skills_dir.mkdir(parents=True, exist_ok=True)
            shutil.move(str(stage_dir), str(paths.skill_dir))
        except Exception:
            if paths.skill_dir.exists() or paths.skill_dir.is_symlink():
                _remove_path(paths.skill_dir)
            if backup is not None:
                shutil.move(str(backup), str(paths.skill_dir))
            raise
        else:
            if backup is not None:
                _remove_path(backup)
    ok, problem = inspect_skill(paths.skill_dir)
    if not ok:
        raise SetupError(problem or "Installed skill failed validation.")
    return replaced


def prune_empty(paths: Sequence[Path]) -> None:
    for path in paths:
        try:
            path.rmdir()
        except OSError:
            break


def _warn_python3() -> None:
    if shutil.which("python3") is None:
        Ui.warn("`python3` is not on PATH.")
        Ui.hint("The skill manifest expects `python3` for bundled scripts. Install Python 3 or adjust your host environment before using the skill.")


def do_install(args: argparse.Namespace) -> int:
    Ui.rule("reCamera Intellisense Skill — install")
    _warn_python3()
    destination = candidate_from_paths(
        explicit_paths(args.path),
        "custom path",
        f"resolved from {args.path}",
        True,
    ) if args.path else choose_destination(discover_destinations(Path.cwd()))
    anchor_missing = not destination.paths.anchor.exists()
    skills_missing = not destination.paths.skills_dir.exists()
    if destination.state in {"installed", "incomplete"}:
        if destination.state == "installed":
            Ui.info(f"Skill already present: {Ui.cyan(str(destination.paths.skill_dir))}")
        else:
            Ui.warn(f"Existing skill directory looks incomplete: {Ui.cyan(str(destination.paths.skill_dir))}")
            if destination.problem:
                Ui.hint(destination.problem)
        if not args.force and not Ui.ask_yes_no("Replace the existing skill?", default=False):
            Ui.info("Install skipped.")
            if destination.state == "installed":
                print(f"SKILL_PATH={destination.paths.skill_dir}")
            return 0
    with source_bundle(args.version, args.force_download) as src:
        Ui.info(f"Source: {src.label}")
        replaced = install_bundle(src.bundle_dir, destination.paths)
    Ui.rule("Installation complete")
    print(f"  Skill:   {Ui.cyan(str(destination.paths.skill_dir))}")
    print(f"  Source:  {src.label}")
    print(f"  Target:  {destination.label}")
    print(f"  Status:  {Ui.green('updated' if replaced else 'installed')}")
    if anchor_missing or skills_missing:
        print()
        Ui.warn("If your assistant is already running, you may need to restart or reload it so it notices the new skills directory.")
    print()
    print(f"SKILL_PATH={destination.paths.skill_dir}")
    return 0


def do_check(args: argparse.Namespace) -> int:
    if args.path:
        candidate = candidate_from_paths(explicit_paths(args.path), "custom path", "explicit check", True)
        if candidate.state == "installed":
            print(candidate.paths.skill_dir)
            return 0
        return 1
    for candidate in discover_destinations(Path.cwd()):
        if candidate.state == "installed":
            print(candidate.paths.skill_dir)
            return 0
    return 1


def _uninstall_targets(args: argparse.Namespace) -> list[Candidate]:
    if args.path:
        candidate = candidate_from_paths(explicit_paths(args.path), "custom path", "explicit uninstall", True)
        return [candidate] if candidate.state in {"installed", "incomplete"} else []
    return [candidate for candidate in discover_destinations(Path.cwd()) if candidate.state in {"installed", "incomplete"}]


def do_uninstall(args: argparse.Namespace) -> int:
    Ui.rule("reCamera Intellisense Skill — uninstall")
    targets = _uninstall_targets(args)
    if not targets:
        Ui.warn("No installed skill found.")
        return 0
    removed_any = False
    for candidate in targets:
        prompt = f"Remove skill at {Ui.cyan(str(candidate.paths.skill_dir))}?"
        if not Ui.ask_yes_no(prompt, default=True):
            Ui.info(f"Skipped {candidate.paths.skill_dir}")
            continue
        try:
            _remove_path(candidate.paths.skill_dir)
            prune_empty((candidate.paths.skills_dir, candidate.paths.anchor))
            Ui.info(f"Removed {candidate.paths.skill_dir}")
            removed_any = True
        except OSError as exc:
            Ui.warn(f"Could not remove {candidate.paths.skill_dir}: {exc}")
    print()
    if removed_any:
        Ui.info("Uninstall complete.")
    else:
        Ui.warn("Nothing was removed.")
    return 0


def do_list_destinations(_: argparse.Namespace) -> int:
    Ui.rule("Candidate skill destinations")
    for index, candidate in enumerate(discover_destinations(Path.cwd()), start=1):
        suffix = f" {Ui.green('(recommended)')}" if candidate.recommended else ""
        print(f"  {index}. {Ui.bold(candidate.label)} {candidate.paths.anchor}{suffix}")
        print(f"     skill:  {candidate.paths.skill_dir}")
        print(f"     state:  {_format_state(candidate.state)}")
        print(f"     note:   {candidate.reason}")
        if candidate.problem and candidate.state == "incomplete":
            print(f"     issue:  {candidate.problem}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="setup-skill.py",
        description="Install the reCamera Intellisense skill into a compatible skills directory.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  setup-skill.py install\n"
            "  setup-skill.py install -y\n"
            "  setup-skill.py install --path ~/.openclaw\n"
            "  setup-skill.py install --version v2.0.0\n"
            "  setup-skill.py check\n"
            "  setup-skill.py uninstall -y\n"
            "  setup-skill.py list-destinations\n"
        ),
    )
    parser.add_argument("--no-colour", "--no-color", dest="no_colour", action="store_true", help="Disable ANSI colour output (same as NO_COLOR=1).")
    sub = parser.add_subparsers(dest="command", metavar="<command>")

    def add_yes(p: argparse.ArgumentParser) -> None:
        p.add_argument("-y", "--yes", action="store_true", help="Non-interactive mode: accept default prompts.")

    install_p = sub.add_parser("install", help="Install the skill into a detected or specified skills directory.")
    install_p.add_argument("--path", metavar="DIR", help="Destination root, a config root like ~/.claude, a skills directory, or the final skill directory.")
    install_p.add_argument("--version", metavar="TAG", help="Install from a specific GitHub release tag instead of the default source.")
    install_p.add_argument("--force-download", action="store_true", help="Download release sources even when a local checkout is available.")
    install_p.add_argument("--force", action="store_true", help="Replace an existing skill directory without prompting.")
    add_yes(install_p)
    install_p.set_defaults(func=do_install)

    check_p = sub.add_parser("check", help="Print the installed skill path and exit 0, else exit 1.")
    check_p.add_argument("--path", metavar="DIR", help="Check only the specified destination root or skill directory.")
    check_p.set_defaults(func=do_check)

    uninstall_p = sub.add_parser("uninstall", help="Remove the installed skill from one or more destinations.")
    uninstall_p.add_argument("--path", metavar="DIR", help="Remove only the specified destination root or skill directory.")
    add_yes(uninstall_p)
    uninstall_p.set_defaults(func=do_uninstall)

    list_p = sub.add_parser("list-destinations", help="List candidate install destinations and their status.")
    list_p.set_defaults(func=do_list_destinations)
    return parser


SUBCOMMANDS = {"install", "check", "uninstall", "list-destinations"}


def normalise_argv(argv: list[str]) -> list[str]:
    if "--check" in argv:
        return ["check", *[arg for arg in argv if arg != "--check"]]
    has_subcommand = any((not arg.startswith("-")) and arg in SUBCOMMANDS for arg in argv)
    if not has_subcommand and not any(arg in {"-h", "--help"} for arg in argv):
        return ["install", *argv]
    return argv


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)

    def _sigint(_signum, _frame) -> None:
        print()
        Ui.warn("Interrupted.")
        sys.exit(130)

    signal.signal(signal.SIGINT, _sigint)
    parser = build_parser()
    args = parser.parse_args(normalise_argv(argv))
    if getattr(args, "no_colour", False):
        Ui.colour = False
    Ui.auto_yes = bool(getattr(args, "yes", False)) or not sys.stdin.isatty()
    if not args.command:
        parser.print_help()
        return 2
    try:
        return args.func(args)
    except SetupError as exc:
        Ui.error(str(exc))
        if exc.hint:
            Ui.hint(exc.hint)
        return exc.exit_code
    except KeyboardInterrupt:
        print()
        Ui.warn("Interrupted.")
        return 130


if __name__ == "__main__":
    sys.exit(main())
