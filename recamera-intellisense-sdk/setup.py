"""Setuptools shim.

Metadata lives in ``pyproject.toml``; this file only wires a build-time hook
that keeps ``skills/recamera-intellisense/scripts/recamera_intellisense`` pointing
at the SDK package, so the skill works without installing the SDK.
"""

from __future__ import annotations

import os
import pathlib
import sys

from setuptools import setup
from setuptools.command.build_py import build_py
from setuptools.command.develop import develop


def _link_skill_package() -> None:
    here = pathlib.Path(__file__).resolve().parent
    src = here / "recamera_intellisense"
    if not src.is_dir():
        return
    target = (
        here.parent
        / "skills"
        / "recamera-intellisense"
        / "scripts"
        / "recamera_intellisense"
    )
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        print(f"[setup.py] skip skill symlink: {exc}", file=sys.stderr)
        return

    rel = os.path.relpath(src, target.parent)
    if target.is_symlink():
        try:
            if os.readlink(target) == rel:
                return
        except OSError:
            pass
        target.unlink()
    elif target.exists():
        # Refuse to clobber a real directory or file we didn't create.
        print(
            f"[setup.py] skip skill symlink: {target} exists and is not a symlink",
            file=sys.stderr,
        )
        return

    try:
        target.symlink_to(rel, target_is_directory=True)
        print(f"[setup.py] linked {target} -> {rel}")
    except OSError as exc:
        print(f"[setup.py] could not create skill symlink: {exc}", file=sys.stderr)


class _BuildPy(build_py):
    def run(self):
        _link_skill_package()
        super().run()


class _Develop(develop):
    def run(self):
        _link_skill_package()
        super().run()


if __name__ == "__main__":
    # Fire the hook on bare ``python setup.py`` invocations too.
    _link_skill_package()

# Ensure setuptools reads this package's pyproject.toml / finds the right
# source tree even when invoked from a different working directory (e.g.
# ``python recamera-intellisense-sdk/setup.py build_py`` from the repo root).
# Without this, setuptools falls back to flat-layout auto-discovery in the
# caller's CWD and fails with "Multiple top-level packages discovered".
os.chdir(str(pathlib.Path(__file__).resolve().parent))

setup(cmdclass={"build_py": _BuildPy, "develop": _Develop})
