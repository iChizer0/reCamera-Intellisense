#!/usr/bin/env python3
"""
reCamera Intellisense — Skill Installer

Downloads the repo tarball and installs the ``skills/recamera-intellisense``
directory into a location of your choice.

    python3 install-skill.py                      # interactive
    python3 install-skill.py install -y           # non-interactive (recommended path)
    python3 install-skill.py install --path ~/.openclaw
    python3 install-skill.py list-paths           # show candidate destinations
    python3 install-skill.py uninstall --path ~/.openclaw

Or via curl (defaults to `install --yes`):

    curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/install-skill.py \
      | python3 - install --yes

Zero third-party dependencies — standard library only.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import shutil
import signal
import sys
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

# ── Constants ─────────────────────────────────────────────────────────

REPO_OWNER = "iChizer0"
REPO_NAME = "reCamera-Intellisense"
DEFAULT_BRANCH = "main"
SKILL_NAME = "recamera-intellisense"
SKILL_SUBDIR = Path("skills") / SKILL_NAME

DOWNLOAD_TIMEOUT = 60


# ══════════════════════════════════════════════════════════════════════
# Presentation layer
# ══════════════════════════════════════════════════════════════════════


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
    def bold(cls, t: str) -> str:
        return cls._wrap("1", t)

    @classmethod
    def dim(cls, t: str) -> str:
        return cls._wrap("2", t)

    @classmethod
    def green(cls, t: str) -> str:
        return cls._wrap("32", t)

    @classmethod
    def yellow(cls, t: str) -> str:
        return cls._wrap("33", t)

    @classmethod
    def red(cls, t: str) -> str:
        return cls._wrap("1;31", t)

    @classmethod
    def cyan(cls, t: str) -> str:
        return cls._wrap("36", t)

    @classmethod
    def info(cls, msg: str) -> None:
        print(f" {cls.green('✓')} {msg}")

    @classmethod
    def step(cls, msg: str) -> None:
        print(f" {cls.cyan('→')} {msg}")

    @classmethod
    def warn(cls, msg: str) -> None:
        print(f" {cls.yellow('!')} {msg}")

    @classmethod
    def error(cls, msg: str) -> None:
        print(f" {cls.red('✗')} {msg}", file=sys.stderr)

    @classmethod
    def hint(cls, msg: str) -> None:
        print(f"   {cls.dim('hint:')} {msg}", file=sys.stderr)

    @classmethod
    def rule(cls, title: str = "") -> None:
        bar = "─" * 60
        if title:
            print()
            print(cls.bold(bar))
            print(cls.bold(f"  {title}"))
            print(cls.bold(bar))
        else:
            print(cls.dim(bar))

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
        return raw in ("y", "yes")

    @classmethod
    def ask_choice(cls, prompt: str, options: list[str], default: int = 1) -> int:
        if cls.auto_yes or not sys.stdin.isatty():
            return default
        try:
            raw = input(f" ? {prompt} [{default}]: ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return default
        if not raw:
            return default
        if not raw.isdigit():
            raise InstallError(
                f"Invalid selection: {raw!r}", hint="Enter one of the numbered options."
            )
        n = int(raw)
        if n < 1 or n > len(options):
            raise InstallError(
                f"Invalid selection: {n}", hint=f"Expected 1..{len(options)}."
            )
        return n


class InstallError(Exception):
    def __init__(self, message: str, hint: str | None = None, exit_code: int = 1):
        super().__init__(message)
        self.hint = hint
        self.exit_code = exit_code


# ══════════════════════════════════════════════════════════════════════
# Destination resolution
# ══════════════════════════════════════════════════════════════════════


@dataclass(frozen=True)
class Destination:
    """Where to install. `base` is the host dir; `target` = base/skills/<name>."""

    base: Path
    target: Path
    label: str

    @classmethod
    def from_base(cls, base: Path, label: str) -> "Destination":
        base = base.expanduser().resolve()
        # If the user pointed directly at the skill directory, use as-is.
        if base.name == SKILL_NAME:
            return cls(base=base.parent, target=base, label=label)
        return cls(base=base, target=base / SKILL_SUBDIR, label=label)


def detect_claw_dirs() -> list[Path]:
    """Auto-discover OpenClaw-style host directories in $HOME."""
    home = Path.home()
    found: list[Path] = []
    for p in sorted(home.glob(".*claw")):
        if p.is_dir() and p.name not in {".", ".."}:
            found.append(p)
    return found


def candidate_destinations() -> list[Destination]:
    """Ordered list of candidate install bases; first is recommended."""
    dests: list[Destination] = [
        Destination.from_base(Path.cwd(), label="current workspace"),
    ]
    for d in detect_claw_dirs():
        dests.append(Destination.from_base(d, label="detected claw dir"))
    return dests


def choose_destination(provided: str | None) -> Destination:
    if provided:
        return Destination.from_base(Path(provided), label="provided")

    options = candidate_destinations()
    if Ui.auto_yes or not sys.stdin.isatty():
        Ui.info(f"Auto-selected: {Ui.cyan(str(options[0].target))}")
        return options[0]

    print()
    print(f"Where should the {Ui.bold(SKILL_NAME)} skill be installed?")
    print()
    for i, dest in enumerate(options, start=1):
        marker = Ui.green("[recommended]") if i == 1 else Ui.dim(f"[{dest.label}]")
        print(f"  {Ui.bold(str(i))}) {dest.target}  {marker}")
    custom_idx = len(options) + 1
    print(f"  {Ui.bold(str(custom_idx))}) Enter a custom path")
    print()

    chosen = Ui.ask_choice(
        "Select an option", [str(i) for i in range(1, custom_idx + 1)], default=1
    )
    if chosen == custom_idx:
        try:
            custom = input(" ? Enter install path: ").strip()
        except (EOFError, KeyboardInterrupt):
            raise InstallError("No path entered.")
        if not custom:
            raise InstallError("No path entered.")
        return Destination.from_base(Path(custom), label="custom")
    return options[chosen - 1]


# ══════════════════════════════════════════════════════════════════════
# Download + extraction
# ══════════════════════════════════════════════════════════════════════


def tarball_url(ref: str) -> str:
    # Accept a branch ("main") or an annotated tag ("v1.2.0").
    return f"https://github.com/{REPO_OWNER}/{REPO_NAME}/archive/{ref}.tar.gz"


def download_tarball(ref: str) -> bytes:
    url = tarball_url(ref)
    Ui.step(f"Downloading {REPO_OWNER}/{REPO_NAME}@{Ui.bold(ref)}…")
    req = Request(url, headers={"User-Agent": "recamera-install-skill"})
    try:
        with urlopen(req, timeout=DOWNLOAD_TIMEOUT) as resp:
            return resp.read()
    except HTTPError as e:
        if e.code == 404:
            raise InstallError(
                f"Cannot find ref {ref!r} on {REPO_OWNER}/{REPO_NAME}.",
                hint="Check --ref (branch or tag). Default is 'main'.",
            ) from None
        raise InstallError(f"Download failed: HTTP {e.code}") from None
    except URLError as e:
        raise InstallError(
            f"Network error downloading tarball: {e.reason}",
            hint="Check your internet connection and proxy settings.",
        ) from None


def _strip_top_prefix(name: str) -> str:
    """Remove the ``{repo}-{ref}/`` top level that GitHub adds."""
    parts = Path(name).parts
    return str(Path(*parts[1:])) if len(parts) > 1 else ""


def extract_skill(blob: bytes, stage: Path) -> None:
    """Extract only skills/<SKILL_NAME>/** from the repo tarball into `stage`."""
    skill_prefix = str(SKILL_SUBDIR).replace("\\", "/") + "/"
    prefix_len = len(SKILL_SUBDIR.parts)
    matched = 0

    with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as tf:
        for member in tf.getmembers():
            if not member.name:
                continue
            rel = _strip_top_prefix(member.name).replace("\\", "/")
            if not rel.startswith(skill_prefix):
                continue

            # Defence in depth — reject traversal even though we trust the source.
            rel_parts = Path(rel).parts
            if ".." in rel_parts or rel.startswith("/"):
                continue
            inner = Path(*rel_parts[prefix_len:])
            if not str(inner):
                continue
            target = stage / inner

            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile():
                continue

            target.parent.mkdir(parents=True, exist_ok=True)
            src = tf.extractfile(member)
            if src is None:
                continue
            with src, open(target, "wb") as out:
                shutil.copyfileobj(src, out)
            # Preserve executable bit for scripts/
            if member.mode & 0o111:
                target.chmod(target.stat().st_mode | 0o111)
            matched += 1

    if matched == 0:
        raise InstallError(
            "Skill files were not found in the downloaded tarball.",
            hint="Repository layout may have changed; please file an issue.",
        )


def atomic_replace_tree(src: Path, target: Path) -> None:
    """Replace `target` with `src` as atomically as the OS allows."""
    target.parent.mkdir(parents=True, exist_ok=True)
    if not target.exists():
        shutil.copytree(src, target)
        return
    backup = target.with_suffix(target.suffix + ".old")
    if backup.exists():
        shutil.rmtree(backup)
    os.rename(target, backup)
    try:
        shutil.copytree(src, target)
    except Exception:
        # Restore on failure
        if target.exists():
            shutil.rmtree(target)
        os.rename(backup, target)
        raise
    else:
        shutil.rmtree(backup)


# ══════════════════════════════════════════════════════════════════════
# Subcommand handlers
# ══════════════════════════════════════════════════════════════════════


def do_install(args: argparse.Namespace) -> int:
    Ui.rule("reCamera Intellisense — Skill Installer")

    dest = choose_destination(args.path)
    Ui.info(f"Install target: {Ui.cyan(str(dest.target))}")

    overwrite = bool(args.force)
    if dest.target.exists() and not overwrite:
        Ui.warn(f"Directory already exists: {dest.target}")
        overwrite = Ui.ask_yes_no(
            "Overwrite (previous copy is backed up)?", default=False
        )
        if not overwrite:
            Ui.info("Aborted — nothing changed.")
            return 0

    blob = download_tarball(args.ref)

    Ui.step("Extracting skill files…")
    with tempfile.TemporaryDirectory() as tmp:
        stage = Path(tmp) / "stage"
        stage.mkdir()
        extract_skill(blob, stage)
        atomic_replace_tree(stage, dest.target)

    Ui.info(f"Skill installed at {Ui.cyan(str(dest.target))}")
    _print_next_steps(dest.target)
    return 0


def _print_next_steps(target: Path) -> None:
    print()
    print(f"  {Ui.bold('Next steps:')}")
    print(f"   • Verify contents:  {Ui.dim(f'ls -la {target}')}")
    print(f"   • View the skill:   {Ui.dim(f'cat {target}/SKILL.md')}")
    print()


def do_list_paths(_: argparse.Namespace) -> int:
    Ui.rule("Candidate install destinations")
    for i, dest in enumerate(candidate_destinations(), start=1):
        marker = Ui.green("[recommended]") if i == 1 else Ui.dim(f"[{dest.label}]")
        exists = Ui.green("exists") if dest.target.exists() else Ui.dim("not present")
        print(f"  {Ui.bold(str(i))}) {dest.target}  {marker}  {exists}")
    print()
    return 0


def do_uninstall(args: argparse.Namespace) -> int:
    Ui.rule("reCamera Intellisense — Skill Uninstaller")
    if args.path:
        dest = Destination.from_base(Path(args.path), label="provided")
    else:
        # Scan candidates, uninstall any that are present.
        hits = [d for d in candidate_destinations() if d.target.exists()]
        if not hits:
            Ui.warn("No skill installations found in default locations.")
            Ui.hint("Pass --path to uninstall from a specific location.")
            return 0
        if len(hits) == 1:
            dest = hits[0]
        else:
            print()
            print("Multiple installations found:")
            for i, d in enumerate(hits, start=1):
                print(f"  {Ui.bold(str(i))}) {d.target}")
            chosen = Ui.ask_choice(
                "Remove which?", [str(i) for i in range(1, len(hits) + 1)], 1
            )
            dest = hits[chosen - 1]

    if not dest.target.exists():
        Ui.warn(f"Not installed: {dest.target}")
        return 0

    if not Ui.ask_yes_no(f"Remove {Ui.cyan(str(dest.target))}?", default=True):
        Ui.info("Aborted — nothing changed.")
        return 0

    try:
        shutil.rmtree(dest.target)
    except OSError as e:
        raise InstallError(f"Failed to remove directory: {e}") from None
    Ui.info(f"Removed {dest.target}")
    return 0


# ══════════════════════════════════════════════════════════════════════
# CLI
# ══════════════════════════════════════════════════════════════════════


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="install-skill.py",
        description="Install the reCamera Intellisense skill directory.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  install-skill.py                      (interactive install)\n"
            "  install-skill.py install -y           (auto-select recommended path)\n"
            "  install-skill.py install --path ~/.openclaw\n"
            "  install-skill.py list-paths\n"
            "  install-skill.py uninstall --path ~/.openclaw\n"
        ),
    )
    parser.add_argument(
        "--no-colour",
        action="store_true",
        help="Disable ANSI colour output (same as NO_COLOR=1).",
    )
    sub = parser.add_subparsers(dest="command", metavar="<command>")

    install_p = sub.add_parser(
        "install",
        help="Download and install the skill directory.",
        description="Download the repo tarball and install the skill directory.",
    )
    install_p.add_argument(
        "--path",
        metavar="PATH",
        help="Custom install base (e.g. ~/.openclaw). "
        "If it ends in /" + SKILL_NAME + " it is used as-is.",
    )
    install_p.add_argument(
        "--ref",
        default=DEFAULT_BRANCH,
        metavar="REF",
        help=f"Branch or tag to install (default: {DEFAULT_BRANCH}).",
    )
    install_p.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="Non-interactive mode: accept all prompts.",
    )
    install_p.add_argument(
        "--force",
        action="store_true",
        help="Overwrite an existing destination without prompting.",
    )
    install_p.set_defaults(func=do_install)

    list_p = sub.add_parser(
        "list-paths",
        help="Print candidate install destinations for the current host.",
    )
    list_p.set_defaults(func=do_list_paths)

    uninstall_p = sub.add_parser(
        "uninstall",
        help="Remove an installed skill directory.",
    )
    uninstall_p.add_argument(
        "--path", metavar="PATH", help="Path to remove. Omit to auto-detect."
    )
    uninstall_p.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="Non-interactive mode: accept all prompts.",
    )
    uninstall_p.set_defaults(func=do_uninstall)

    return parser


def _normalise_argv(argv: list[str]) -> list[str]:
    subcommands = {"install", "list-paths", "uninstall"}
    has_sub = any((not a.startswith("-")) and a in subcommands for a in argv)
    if not has_sub and not any(a in ("-h", "--help") for a in argv):
        return ["install", *argv]
    return argv


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)

    def _sigint(_signum, _frame):
        print()
        Ui.warn("Interrupted.")
        sys.exit(130)

    signal.signal(signal.SIGINT, _sigint)

    parser = _build_parser()
    argv = _normalise_argv(argv)
    args = parser.parse_args(argv)

    if getattr(args, "no_colour", False):
        Ui.colour = False
    Ui.auto_yes = bool(getattr(args, "yes", False)) or not sys.stdin.isatty()

    if not args.command:
        parser.print_help()
        return 2

    try:
        return args.func(args)
    except InstallError as e:
        Ui.error(str(e))
        if e.hint:
            Ui.hint(e.hint)
        return e.exit_code
    except KeyboardInterrupt:
        print()
        Ui.warn("Interrupted.")
        return 130


if __name__ == "__main__":
    sys.exit(main())
