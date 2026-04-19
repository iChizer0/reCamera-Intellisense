#!/usr/bin/env python3
"""
reCamera Intellisense Skill Installer

Downloads the repository tarball and installs the `skills/recamera-intellisense`
skill directory to a selected destination.

Usage:
    python3 install-skill.py                 # interactive
    python3 install-skill.py --yes           # non-interactive (uses recommended path)
    python3 install-skill.py --path /tmp/x   # install to custom base path
    curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/install-skill.py | python3
"""

from __future__ import annotations

import argparse
import io
import shutil
import sys
import tarfile
import tempfile
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import urlopen

REPO_OWNER = "iChizer0"
REPO_NAME = "reCamera-Intellisense"
REPO_BRANCH = "main"
SKILL_NAME = "recamera-intellisense"
SKILL_SUBDIR = Path("skills") / SKILL_NAME
TARBALL_URL = (
    f"https://github.com/{REPO_OWNER}/{REPO_NAME}/archive/refs/heads/{REPO_BRANCH}.tar.gz"
)

_COLOUR = sys.stdout.isatty()


def _fmt(code: str, text: str) -> str:
    return f"\033[{code}m{text}\033[0m" if _COLOUR else text


def info(msg: str) -> None:
    print(f"{_fmt('1;34', '[info]')}  {msg}")


def ok(msg: str) -> None:
    print(f"{_fmt('1;32', '[ok]')}    {msg}")


def warn(msg: str) -> None:
    print(f"{_fmt('1;33', '[warn]')}  {msg}")


def error(msg: str) -> None:
    print(f"{_fmt('1;31', '[error]')} {msg}", file=sys.stderr)
    raise SystemExit(1)


def ask_yes_no(prompt: str, default: bool, auto_yes: bool) -> bool:
    if auto_yes or not sys.stdin.isatty():
        return default
    hint = "Y/n" if default else "y/N"
    try:
        answer = input(f"{prompt} [{hint}]: ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        print()
        return default
    if not answer:
        return default
    return answer in ("y", "yes")


def detect_claw_dirs() -> list[Path]:
    home = Path.home()
    return sorted(
        p for p in home.glob(".*claw") if p.is_dir() and p.name != "."
    )


def normalize_destination(selection: Path) -> Path:
    if selection.name == SKILL_NAME:
        return selection
    return selection / SKILL_SUBDIR


def choose_install_path(auto_yes: bool, provided_path: str | None) -> Path:
    workspace_dir = Path.cwd()

    if provided_path:
        selected = normalize_destination(Path(provided_path).expanduser())
        info(f"Using provided path: {selected}")
        return selected

    options: list[Path] = [workspace_dir / SKILL_SUBDIR]
    info(f"Detected workspace: {workspace_dir}")

    for d in detect_claw_dirs():
        options.append(d / SKILL_SUBDIR)
        info(f"Detected claw directory: {d}")

    if auto_yes or not sys.stdin.isatty():
        info(f"Auto-selected install path: {options[0]}")
        return options[0]

    print()
    print(f"Where would you like to install the '{SKILL_NAME}' skill?")
    print()

    for i, option in enumerate(options, start=1):
        if i == 1:
            print(f"  {_fmt('1;32', f'[{i}] {option}  (recommended)')}")
        else:
            print(f"  [{i}] {option}")
    custom_idx = len(options) + 1
    print(f"  [{custom_idx}] Enter a custom path")
    print()

    raw = input("Select an option [1]: ").strip() or "1"
    if not raw.isdigit():
        error(f"Invalid selection: {raw}")
    choice = int(raw)
    if choice < 1 or choice > custom_idx:
        error(f"Invalid selection: {choice}")

    if choice == custom_idx:
        custom = input("Enter the install path: ").strip()
        if not custom:
            error("No path provided")
        return normalize_destination(Path(custom).expanduser())

    return options[choice - 1]


def _strip_repo_prefix(member_name: str) -> str:
    parts = Path(member_name).parts
    if not parts:
        return ""
    # Remove top-level "{repo}-{branch}/"
    return str(Path(*parts[1:])) if len(parts) > 1 else ""


def download_and_install(dest: Path, overwrite: bool) -> None:
    info(f"Downloading {REPO_OWNER}/{REPO_NAME}@{REPO_BRANCH} ...")
    try:
        with urlopen(TARBALL_URL, timeout=60) as resp:
            blob = resp.read()
    except HTTPError as exc:
        error(f"Failed to download tarball (HTTP {exc.code}) from {TARBALL_URL}")
    except URLError as exc:
        error(f"Failed to download tarball from {TARBALL_URL}: {exc.reason}")

    info("Extracting skill files ...")

    if dest.exists() and overwrite:
        shutil.rmtree(dest)

    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        stage = tmpdir / "stage"
        stage.mkdir(parents=True, exist_ok=True)

        skill_prefix = str(SKILL_SUBDIR).replace("\\", "/") + "/"

        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as tf:
            matched = [
                m
                for m in tf.getmembers()
                if m.name
                and _strip_repo_prefix(m.name).replace("\\", "/").startswith(skill_prefix)
            ]
            if not matched:
                error(
                    "Failed to find skill files in tarball. "
                    "Repository layout may have changed."
                )

            for member in matched:
                rel = _strip_repo_prefix(member.name)
                rel_parts = Path(rel).parts
                rel_after_prefix = Path(*rel_parts[len(SKILL_SUBDIR.parts) :])
                target = stage / rel_after_prefix

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

        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(stage, dest, dirs_exist_ok=True)

    ok(f"Skill installed to: {dest}")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Install the reCamera Intellisense skill directory.",
    )
    parser.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="Non-interactive mode: use recommended destination and overwrite if needed.",
    )
    parser.add_argument(
        "--path",
        help=(
            "Custom base install path (e.g. ~/.my-skills or ~/.my-skills/recamera-intellisense)."
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite existing destination without prompting.",
    )
    args = parser.parse_args()

    print()
    print("========================================")
    print(" reCamera Intellisense Skill Installer")
    print("========================================")
    print()

    auto_yes = args.yes
    dest = choose_install_path(auto_yes=auto_yes, provided_path=args.path)

    print()

    overwrite = args.force or auto_yes
    if dest.exists() and not overwrite:
        warn(f"Directory already exists: {dest}")
        overwrite = ask_yes_no("Overwrite?", default=False, auto_yes=auto_yes)
        if not overwrite:
            info("Aborted.")
            return

    download_and_install(dest=dest, overwrite=overwrite)

    print()
    info("You can verify the installation with:")
    print(f"  ls -la {dest}")
    print()


if __name__ == "__main__":
    main()
