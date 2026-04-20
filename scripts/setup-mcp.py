#!/usr/bin/env python3
"""
reCamera Intellisense MCP Server — Setup

Auto-detects platform, downloads the latest release binary from GitHub,
installs it under ~/.recamera/bin, and optionally configures detected
MCP clients (VS Code, Claude Desktop, Claude Code, Cursor, Windsurf, Nanobot).

    python3 setup-mcp.py                 # interactive install
    python3 setup-mcp.py install -y      # non-interactive install
    python3 setup-mcp.py check           # print binary path (exit 0) or exit 1
    python3 setup-mcp.py uninstall -y    # remove binary + deregister clients
    python3 setup-mcp.py list-clients    # show supported MCP clients
    python3 setup-mcp.py configure --client cursor  # skip download, reconfigure

Or pipe from curl (defaults to non-interactive `install --yes`):

    curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py \
      | python3 - install --yes

Zero third-party dependencies — standard library only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import signal
import stat
import sys
import tarfile
import tempfile
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

# ── Constants ─────────────────────────────────────────────────────────

REPO = "iChizer0/reCamera-Intellisense"
BINARY_NAME = "recamera-intellisense-mcp"
DEFAULT_INSTALL_DIR = Path.home() / ".recamera" / "bin"
SERVER_KEY = "recamera"

# (system, machine) → GitHub release asset stem
PLATFORM_MAP: dict[tuple[str, str], str] = {
    ("Linux", "x86_64"): "recamera-intellisense-mcp-linux-x86_64",
    ("Linux", "aarch64"): "recamera-intellisense-mcp-linux-aarch64",
    ("Darwin", "arm64"): "recamera-intellisense-mcp-macos-aarch64",
    ("Windows", "AMD64"): "recamera-intellisense-mcp-windows-x86_64",
}

HTTP_TIMEOUT_API = 30
HTTP_TIMEOUT_DOWNLOAD = 180
CHUNK_SIZE = 64 * 1024


# ══════════════════════════════════════════════════════════════════════
# Presentation layer — colours, prompts, errors
# ══════════════════════════════════════════════════════════════════════


def _use_colour() -> bool:
    if os.environ.get("NO_COLOR"):
        return False
    if os.environ.get("FORCE_COLOR"):
        return True
    return sys.stdout.isatty()


class Ui:
    """Tiny ANSI-only presentation helper. All output goes through here."""

    colour = _use_colour()
    auto_yes = False  # populated from CLI args

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
        bar_length = max(60, len(title) + 4)
        padding = " " * ((bar_length - len(title) - 2) // 2)
        bar = "─" * bar_length
        if title:
            print()
            print(cls.bold(bar))
            print(cls.bold(f"{padding}{title}"))
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


class SetupError(Exception):
    """Fatal but actionable error. Prints as a coloured line + optional hint."""

    def __init__(self, message: str, hint: str | None = None, exit_code: int = 1):
        super().__init__(message)
        self.hint = hint
        self.exit_code = exit_code


# ══════════════════════════════════════════════════════════════════════
# Platform + release metadata
# ══════════════════════════════════════════════════════════════════════


@dataclass(frozen=True)
class PlatformTarget:
    system: str
    machine: str
    asset_stem: str
    archive_ext: str  # ".tar.gz" or ".zip"

    @property
    def binary_filename(self) -> str:
        return BINARY_NAME + (".exe" if self.system == "Windows" else "")

    @property
    def asset_name(self) -> str:
        return self.asset_stem + self.archive_ext


def detect_platform() -> PlatformTarget:
    system = platform.system()
    machine = platform.machine()
    stem = PLATFORM_MAP.get((system, machine))
    if stem is None:
        supported = "\n    ".join(f"{s} {m}" for (s, m) in PLATFORM_MAP)
        raise SetupError(
            f"Unsupported platform: {system} {machine}",
            hint=f"Supported targets:\n    {supported}",
        )
    ext = ".zip" if system == "Windows" else ".tar.gz"
    Ui.info(f"Platform: {Ui.bold(f'{system} {machine}')}")
    return PlatformTarget(system, machine, stem, ext)


def _github_get(url: str, *, timeout: int) -> dict:
    req = Request(url, headers={"Accept": "application/vnd.github+json"})
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except HTTPError as e:
        if e.code == 404:
            raise SetupError(
                "GitHub release not found.",
                hint="The repository may not have published a release yet, "
                "or the tag passed via --version does not exist.",
            ) from None
        if e.code in (401, 403):
            raise SetupError(
                f"GitHub API rejected the request (HTTP {e.code}).",
                hint="If rate-limited, set GITHUB_TOKEN in the environment.",
            ) from None
        raise SetupError(f"GitHub API error: HTTP {e.code}") from None
    except URLError as e:
        raise SetupError(
            f"Network error contacting GitHub: {e.reason}",
            hint="Check your internet connection and any HTTP_PROXY settings.",
        ) from None


def fetch_release(version: str | None) -> dict:
    if version:
        Ui.step(f"Fetching release {Ui.bold(version)} from GitHub…")
        return _github_get(
            f"https://api.github.com/repos/{REPO}/releases/tags/{version}",
            timeout=HTTP_TIMEOUT_API,
        )
    Ui.step("Fetching latest release from GitHub…")
    return _github_get(
        f"https://api.github.com/repos/{REPO}/releases/latest",
        timeout=HTTP_TIMEOUT_API,
    )


def find_asset(release: dict, target: PlatformTarget) -> tuple[str, str]:
    assets = release.get("assets", [])
    for asset in assets:
        if asset.get("name") == target.asset_name:
            return asset["browser_download_url"], asset["name"]

    available = ", ".join(a["name"] for a in assets) or "(none)"
    raise SetupError(
        f"Release {release.get('tag_name', '?')!r} has no asset named "
        f"{target.asset_name!r}.",
        hint=f"Assets in this release: {available}",
    )


def find_checksum_url(release: dict, asset_name: str) -> str | None:
    """Locate a published SHA-256 digest for ``asset_name``.

    Looks for (in order): ``<asset>.sha256``, ``<asset>.sha256sum``,
    ``SHA256SUMS``, ``checksums.txt``. Returns the asset download URL
    or ``None`` if no recognised digest asset exists in the release.
    """
    assets = release.get("assets", [])
    by_name = {a.get("name", ""): a for a in assets}
    for candidate in (f"{asset_name}.sha256", f"{asset_name}.sha256sum"):
        asset = by_name.get(candidate)
        if asset:
            return asset["browser_download_url"]
    for candidate in ("SHA256SUMS", "SHA256SUMS.txt", "checksums.txt"):
        asset = by_name.get(candidate)
        if asset:
            return asset["browser_download_url"]
    return None


# ══════════════════════════════════════════════════════════════════════
# Download + install
# ══════════════════════════════════════════════════════════════════════


def _format_mb(n: int) -> str:
    return f"{n / (1024 * 1024):.1f} MB"


def download_file(url: str, dest: Path) -> None:
    show_progress = sys.stdout.isatty()
    try:
        with urlopen(Request(url), timeout=HTTP_TIMEOUT_DOWNLOAD) as resp:
            total_hdr = resp.headers.get("Content-Length")
            total = int(total_hdr) if total_hdr else 0
            received = 0
            last_pct = -1
            with open(dest, "wb") as f:
                while True:
                    chunk = resp.read(CHUNK_SIZE)
                    if not chunk:
                        break
                    f.write(chunk)
                    received += len(chunk)
                    if show_progress and total:
                        pct = received * 100 // total
                        if pct != last_pct:
                            last_pct = pct
                            print(
                                f"\r   Downloading… {_format_mb(received)} / "
                                f"{_format_mb(total)} ({pct}%)",
                                end="",
                                flush=True,
                            )
            if show_progress:
                print()
    except (HTTPError, URLError) as e:
        raise SetupError(f"Download failed: {e}") from None


def _fetch_text(
    url: str, *, timeout: int = HTTP_TIMEOUT_API, max_bytes: int = 1 << 20
) -> str:
    """Download a small text asset (checksums file). Hard cap to prevent
    unexpectedly huge payloads from exhausting memory."""
    try:
        with urlopen(Request(url), timeout=timeout) as resp:
            data = resp.read(max_bytes + 1)
    except (HTTPError, URLError) as e:
        raise SetupError(f"Failed to download checksum asset: {e}") from None
    if len(data) > max_bytes:
        raise SetupError("Checksum asset exceeds 1 MiB — refusing to parse.")
    return data.decode("utf-8", errors="replace")


def _parse_checksum(text: str, asset_name: str) -> str | None:
    """Extract a lowercase hex SHA-256 digest for ``asset_name``.

    Supports both single-line ``sha256sum``-style files (``<hex>  <name>``)
    and a bare hex digest. Returns ``None`` if no matching line is present.
    """
    stripped = text.strip()
    # Bare digest (``<asset>.sha256`` convention: a single 64-char hex token).
    tokens = stripped.split()
    if len(tokens) == 1 and len(tokens[0]) == 64:
        try:
            int(tokens[0], 16)
            return tokens[0].lower()
        except ValueError:
            pass
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) < 2:
            continue
        digest = parts[0].lstrip("*").lower()
        # sha256sum uses ``*name`` for binary mode, ``name`` for text.
        name = parts[-1].lstrip("*")
        if len(digest) == 64 and name == asset_name:
            try:
                int(digest, 16)
                return digest
            except ValueError:
                continue
    return None


def _sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(CHUNK_SIZE), b""):
            h.update(chunk)
    return h.hexdigest()


def verify_checksum(
    archive: Path, asset_name: str, checksum_url: str | None, *, skip: bool
) -> None:
    """Enforce integrity of ``archive`` against the release's SHA-256 digest.

    Fail-closed policy:
      * If a checksum asset is published and we can parse a digest for
        ``asset_name``, the archive must match — otherwise abort.
      * If no checksum asset is published, abort unless the caller passed
        ``--skip-checksum`` (or ``RECAMERA_SKIP_CHECKSUM=1``); in that case
        we warn loudly and continue.
    """
    actual = _sha256_of(archive)
    if checksum_url is None:
        msg = (
            "Release did not publish a SHA-256 digest alongside "
            f"{asset_name!r}; cannot verify download integrity."
        )
        if not skip:
            raise SetupError(
                msg,
                hint=(
                    "Re-run with --skip-checksum to proceed anyway (not "
                    "recommended on untrusted networks), or set "
                    "RECAMERA_SKIP_CHECKSUM=1. Downloaded digest: "
                    f"sha256={actual}"
                ),
            )
        Ui.warn(msg)
        Ui.warn(f"Proceeding without verification (sha256={actual}).")
        return
    Ui.step("Verifying SHA-256 digest…")
    try:
        text = _fetch_text(checksum_url)
    except SetupError:
        if skip:
            Ui.warn(
                "Failed to fetch published checksum; continuing because "
                "--skip-checksum is set."
            )
            Ui.warn(f"Local sha256={actual}")
            return
        raise
    expected = _parse_checksum(text, asset_name)
    if expected is None:
        if skip:
            Ui.warn(
                "Checksum asset fetched but no entry matched "
                f"{asset_name!r}; continuing because --skip-checksum is set."
            )
            Ui.warn(f"Local sha256={actual}")
            return
        raise SetupError(
            f"Checksum asset at {checksum_url} has no entry for {asset_name!r}.",
            hint=(
                "Release packaging may be inconsistent. Re-run with "
                "--skip-checksum to bypass (at your own risk)."
            ),
        )
    if expected != actual:
        raise SetupError(
            "SHA-256 mismatch — refusing to install.",
            hint=(
                f"Expected {expected}\n"
                f"Got      {actual}\n"
                "The download may be corrupted or tampered with."
            ),
        )
    Ui.info(f"SHA-256 OK ({actual[:16]}…)")


def _safe_tar_member(tar: tarfile.TarFile, wanted: str) -> tarfile.TarInfo:
    for m in tar.getmembers():
        if Path(m.name).name != wanted:
            continue
        parts = Path(m.name).parts
        if m.name.startswith(("/", "..")) or ".." in parts:
            continue
        if m.isfile():
            return m
    raise SetupError(
        f"Archive does not contain expected binary {wanted!r}.",
        hint="Release may be corrupted; try --version to pin an older tag.",
    )


def _safe_zip_name(zf: zipfile.ZipFile, wanted: str) -> str:
    for n in zf.namelist():
        if Path(n).name != wanted:
            continue
        if n.startswith(("/", "..")) or ".." in Path(n).parts:
            continue
        return n
    raise SetupError(
        f"Archive does not contain expected binary {wanted!r}.",
        hint="Release may be corrupted; try --version to pin an older tag.",
    )


def extract_and_install(
    archive: Path, target: PlatformTarget, install_dir: Path
) -> Path:
    install_dir.mkdir(parents=True, exist_ok=True)
    dest = install_dir / target.binary_filename

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        if target.archive_ext == ".tar.gz":
            with tarfile.open(archive, "r:gz") as tar:
                member = _safe_tar_member(tar, target.binary_filename)
                # filter="data" silences Py3.14 DeprecationWarning and hardens
                # extraction (rejects absolute paths, traversal, device nodes).
                try:
                    tar.extract(member, path=tmp_path, filter="data")
                except TypeError:  # Python < 3.12
                    tar.extract(member, path=tmp_path)
                extracted = tmp_path / member.name
        else:
            with zipfile.ZipFile(archive, "r") as zf:
                name = _safe_zip_name(zf, target.binary_filename)
                zf.extract(name, path=tmp_path)
                extracted = tmp_path / name

        tmp_dest = dest.with_suffix(dest.suffix + ".new")
        shutil.move(str(extracted), str(tmp_dest))
        if target.system != "Windows":
            tmp_dest.chmod(
                tmp_dest.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
            )
        os.replace(tmp_dest, dest)

    return dest


# ══════════════════════════════════════════════════════════════════════
# MCP client agents
# ══════════════════════════════════════════════════════════════════════


class Agent:
    """Base class for an MCP client configuration integration."""

    key: str = ""  # short CLI alias, e.g. "vscode"
    name: str = ""  # human-readable name
    config_path: Path | None = None

    def detect(self) -> bool:
        raise NotImplementedError

    def apply(self, cfg: dict, binary_path: Path) -> None:
        raise NotImplementedError

    def is_registered(self, cfg: dict) -> bool:
        raise NotImplementedError

    def deregister(self, cfg: dict) -> None:
        raise NotImplementedError

    def manual_instructions(self, binary_path: Path) -> str:
        raise NotImplementedError

    # Shared IO -------------------------------------------------------
    def config_exists(self) -> bool:
        return self.config_path is not None and self.config_path.exists()

    def read_config(self) -> dict | None:
        """Return parsed dict, {} for missing/empty, None for broken JSON."""
        if not self.config_path or not self.config_path.exists():
            return {}
        try:
            raw = self.config_path.read_text(encoding="utf-8")
        except OSError as e:
            raise SetupError(
                f"Cannot read {self.name} config ({self.config_path}): {e}",
            ) from None
        if not raw.strip():
            return {}
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return None

    def write_config(self, data: dict) -> None:
        assert self.config_path is not None
        self.config_path.parent.mkdir(parents=True, exist_ok=True)
        if self.config_path.exists():
            backup = self.config_path.with_suffix(self.config_path.suffix + ".bak")
            shutil.copy2(self.config_path, backup)
        tmp = self.config_path.with_suffix(self.config_path.suffix + ".tmp")
        tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
        os.replace(tmp, self.config_path)

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        if cfg is None:
            raise SetupError(
                f"{self.name} config is not valid JSON: {self.config_path}",
                hint="Fix or remove the file by hand before re-running — "
                "I refuse to overwrite user data.",
            )
        self.apply(cfg, binary_path)
        self.write_config(cfg)

    def unconfigure(self) -> bool:
        cfg = self.read_config()
        if cfg is None or not cfg:
            return False
        if not self.is_registered(cfg):
            return False
        self.deregister(cfg)
        self.write_config(cfg)
        return True


# ── Concrete agents ───────────────────────────────────────────────────


def _appdata() -> Path:
    return Path(os.environ.get("APPDATA", ""))


def _prune_empty(cfg: dict, *path: str) -> None:
    """Remove `cfg[path[0]][path[1]]...` if, after popping, the inner dict is empty.
    Walks the path and prunes empty containers bottom-up."""
    stack: list[tuple[dict, str]] = []
    node: dict = cfg
    for key in path:
        child = node.get(key)
        if not isinstance(child, dict):
            return
        stack.append((node, key))
        node = child
    while stack:
        parent, key = stack.pop()
        if parent.get(key) == {}:
            parent.pop(key, None)
        else:
            break


class _MCPServersStyle(Agent):
    """Base for clients storing under top-level `mcpServers.<key>`."""

    def is_registered(self, cfg: dict) -> bool:
        return SERVER_KEY in cfg.get("mcpServers", {})

    def deregister(self, cfg: dict) -> None:
        cfg.get("mcpServers", {}).pop(SERVER_KEY, None)
        _prune_empty(cfg, "mcpServers")


class VSCodeAgent(Agent):
    key, name = "vscode", "VS Code"

    def __init__(self) -> None:
        if sys.platform == "darwin":
            self.config_path = (
                Path.home()
                / "Library"
                / "Application Support"
                / "Code"
                / "User"
                / "settings.json"
            )
        elif os.name == "nt":
            self.config_path = _appdata() / "Code" / "User" / "settings.json"
        else:
            self.config_path = (
                Path.home() / ".config" / "Code" / "User" / "settings.json"
            )

    def detect(self) -> bool:
        return shutil.which("code") is not None or self.config_exists()

    def apply(self, cfg: dict, binary_path: Path) -> None:
        cfg.setdefault("mcp", {}).setdefault("servers", {})[SERVER_KEY] = {
            "command": str(binary_path),
        }

    def is_registered(self, cfg: dict) -> bool:
        return SERVER_KEY in cfg.get("mcp", {}).get("servers", {})

    def deregister(self, cfg: dict) -> None:
        cfg.get("mcp", {}).get("servers", {}).pop(SERVER_KEY, None)
        _prune_empty(cfg, "mcp", "servers")
        _prune_empty(cfg, "mcp")

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"Add to settings.json (User or Workspace):\n"
            f'   {{"mcp": {{"servers": {{"{SERVER_KEY}": '
            f'{{"command": "{binary_path}"}}}}}}}}'
        )


class ClaudeDesktopAgent(_MCPServersStyle):
    key, name = "claude-desktop", "Claude Desktop"

    def __init__(self) -> None:
        if sys.platform == "darwin":
            self.config_path = (
                Path.home()
                / "Library"
                / "Application Support"
                / "Claude"
                / "claude_desktop_config.json"
            )
        elif os.name == "nt":
            self.config_path = _appdata() / "Claude" / "claude_desktop_config.json"
        else:
            self.config_path = (
                Path.home() / ".config" / "Claude" / "claude_desktop_config.json"
            )

    def detect(self) -> bool:
        if shutil.which("claude-desktop"):
            return True
        if sys.platform == "darwin" and Path("/Applications/Claude.app").exists():
            return True
        return self.config_exists()

    def apply(self, cfg: dict, binary_path: Path) -> None:
        cfg.setdefault("mcpServers", {})[SERVER_KEY] = {"command": str(binary_path)}

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"Add to claude_desktop_config.json:\n"
            f'   {{"mcpServers": {{"{SERVER_KEY}": '
            f'{{"command": "{binary_path}"}}}}}}'
        )


class ClaudeCodeAgent(_MCPServersStyle):
    key, name = "claude-code", "Claude Code"

    def __init__(self) -> None:
        self.config_path = Path.home() / ".claude.json"

    def detect(self) -> bool:
        return shutil.which("claude") is not None

    def apply(self, cfg: dict, binary_path: Path) -> None:
        cfg.setdefault("mcpServers", {})[SERVER_KEY] = {
            "type": "stdio",
            "command": str(binary_path),
            "args": [],
        }

    def manual_instructions(self, binary_path: Path) -> str:
        return f"Run: claude mcp add --transport stdio {SERVER_KEY} -- {binary_path}"


class CursorAgent(_MCPServersStyle):
    key, name = "cursor", "Cursor"

    def __init__(self) -> None:
        self.config_path = Path.home() / ".cursor" / "mcp.json"

    def detect(self) -> bool:
        if shutil.which("cursor"):
            return True
        if sys.platform == "darwin" and Path("/Applications/Cursor.app").exists():
            return True
        return self.config_exists()

    def apply(self, cfg: dict, binary_path: Path) -> None:
        cfg.setdefault("mcpServers", {})[SERVER_KEY] = {"command": str(binary_path)}

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"Add to ~/.cursor/mcp.json:\n"
            f'   {{"mcpServers": {{"{SERVER_KEY}": '
            f'{{"command": "{binary_path}"}}}}}}'
        )


class WindsurfAgent(_MCPServersStyle):
    key, name = "windsurf", "Windsurf"

    def __init__(self) -> None:
        if os.name == "nt":
            self.config_path = _appdata() / "Codeium" / "Windsurf" / "mcp_config.json"
        else:
            self.config_path = Path.home() / ".codeium" / "windsurf" / "mcp_config.json"

    def detect(self) -> bool:
        if shutil.which("windsurf"):
            return True
        if sys.platform == "darwin" and Path("/Applications/Windsurf.app").exists():
            return True
        return self.config_exists()

    def apply(self, cfg: dict, binary_path: Path) -> None:
        cfg.setdefault("mcpServers", {})[SERVER_KEY] = {"command": str(binary_path)}

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"Add to mcp_config.json:\n"
            f'   {{"mcpServers": {{"{SERVER_KEY}": '
            f'{{"command": "{binary_path}"}}}}}}'
        )


class NanobotAgent(Agent):
    key, name = "nanobot", "Nanobot"

    def __init__(self) -> None:
        self.config_path = Path.home() / ".nanobot" / "config.json"

    def detect(self) -> bool:
        return shutil.which("nanobot") is not None or self.config_exists()

    def apply(self, cfg: dict, binary_path: Path) -> None:
        cfg.setdefault("tools", {}).setdefault("mcpServers", {})[SERVER_KEY] = {
            "command": str(binary_path),
            "args": [],
        }

    def is_registered(self, cfg: dict) -> bool:
        return SERVER_KEY in cfg.get("tools", {}).get("mcpServers", {})

    def deregister(self, cfg: dict) -> None:
        cfg.get("tools", {}).get("mcpServers", {}).pop(SERVER_KEY, None)
        _prune_empty(cfg, "tools", "mcpServers")
        _prune_empty(cfg, "tools")

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"Add to ~/.nanobot/config.json:\n"
            f'   {{"tools": {{"mcpServers": {{"{SERVER_KEY}": '
            f'{{"command": "{binary_path}", "args": []}}}}}}}}'
        )


ALL_AGENT_CLASSES: tuple[type[Agent], ...] = (
    VSCodeAgent,
    ClaudeDesktopAgent,
    ClaudeCodeAgent,
    CursorAgent,
    WindsurfAgent,
    NanobotAgent,
)


def agents_by_key() -> dict[str, Agent]:
    return {cls().key: cls() for cls in ALL_AGENT_CLASSES}


def _resolve_keys(keys: Sequence[str]) -> list[Agent]:
    instances = [cls() for cls in ALL_AGENT_CLASSES]
    known = {a.key: a for a in instances}
    unknown = [k for k in keys if k not in known]
    if unknown:
        raise SetupError(
            f"Unknown client key(s): {', '.join(unknown)}",
            hint=f"Available: {', '.join(sorted(known))}",
        )
    return [known[k] for k in keys]


def filter_agents(keys: Sequence[str] | None) -> list[Agent]:
    """Return detected agents (when `keys` is falsy) or the exact named set."""
    if keys:
        return _resolve_keys(keys)
    return [cls() for cls in ALL_AGENT_CLASSES if cls().detect()]


def all_or_named_agents(keys: Sequence[str] | None) -> list[Agent]:
    """For destructive ops: include every known agent when unfiltered so we
    can still clean up stale registrations from apps no longer installed."""
    if keys:
        return _resolve_keys(keys)
    return [cls() for cls in ALL_AGENT_CLASSES]


# ══════════════════════════════════════════════════════════════════════
# Orchestration
# ══════════════════════════════════════════════════════════════════════


def resolve_installed_binary(install_dir: Path, system: str) -> Path | None:
    name = BINARY_NAME + (".exe" if system == "Windows" else "")
    candidate = install_dir / name
    if candidate.is_file():
        return candidate
    on_path = shutil.which(BINARY_NAME)
    return Path(on_path) if on_path else None


def do_install(args: argparse.Namespace) -> int:
    install_dir = Path(args.dir).expanduser().resolve()
    Ui.rule("reCamera Intellisense MCP Server — install")

    skip_checksum = (
        bool(getattr(args, "skip_checksum", False))
        or os.environ.get("RECAMERA_SKIP_CHECKSUM") == "1"
    )
    if skip_checksum:
        Ui.warn(
            "Checksum verification DISABLED — binary integrity will not be "
            "checked. Only continue on a trusted network."
        )

    target = detect_platform()
    existing = resolve_installed_binary(install_dir, target.system)
    if existing and not args.force_download:
        Ui.info(f"Binary already present: {Ui.cyan(str(existing))}")
        if Ui.ask_yes_no("Re-download and overwrite?", default=False):
            binary_path = _download_and_extract(
                target, install_dir, args.version, skip_checksum=skip_checksum
            )
        else:
            binary_path = existing
    else:
        binary_path = _download_and_extract(
            target, install_dir, args.version, skip_checksum=skip_checksum
        )

    Ui.info(f"Installed to {Ui.cyan(str(binary_path))}")

    agents = filter_agents(args.client)
    failed: list[Agent] = []
    if not agents and not args.client:
        Ui.warn("No supported MCP clients detected on this system.")
    else:
        failed = _configure_agents(agents, binary_path)

    configured = [a for a in agents if a not in failed]
    _print_summary(binary_path, configured, failed)

    print(f"BINARY_PATH={binary_path}")
    return 0


def _download_and_extract(
    target: PlatformTarget,
    install_dir: Path,
    version: str | None,
    *,
    skip_checksum: bool,
) -> Path:
    release = fetch_release(version)
    tag = release.get("tag_name", "unknown")
    Ui.info(f"Release: {Ui.bold(tag)}")

    url, filename = find_asset(release, target)
    checksum_url = find_checksum_url(release, filename)
    with tempfile.TemporaryDirectory() as tmp:
        archive = Path(tmp) / filename
        download_file(url, archive)
        Ui.info(f"Downloaded {filename}")
        verify_checksum(archive, filename, checksum_url, skip=skip_checksum)
        return extract_and_install(archive, target, install_dir)


def _configure_agents(agents: Iterable[Agent], binary_path: Path) -> list[Agent]:
    """Run interactive configuration. Returns the subset whose auto-configure
    failed (broken config, write error, or raised SetupError) — these are
    the only agents we still owe a manual-setup snippet at the end."""
    agents = list(agents)
    if not agents:
        return []

    print()
    Ui.info(f"MCP clients: {', '.join(Ui.bold(a.name) for a in agents)}")

    failed: list[Agent] = []
    for agent in agents:
        cfg = agent.read_config()
        if cfg is None:
            Ui.warn(
                f"{agent.name}: config is not valid JSON "
                f"({agent.config_path}). Skipping — refusing to overwrite."
            )
            Ui.hint("Fix the file by hand then re-run `configure`.")
            failed.append(agent)
            continue

        if agent.is_registered(cfg):
            Ui.info(f"{agent.name}: already configured — skipping")
            continue

        prompt = f"Configure {Ui.bold(agent.name)} ({Ui.dim(str(agent.config_path))})?"
        if not Ui.ask_yes_no(prompt, default=True):
            Ui.info(f"{agent.name}: skipped")
            continue

        try:
            agent.configure(binary_path)
        except SetupError as e:
            Ui.warn(f"{agent.name}: {e}")
            if e.hint:
                Ui.hint(e.hint)
            failed.append(agent)
            continue
        except OSError as e:
            Ui.warn(f"{agent.name}: failed to write config — {e}")
            failed.append(agent)
            continue

        Ui.info(f"{agent.name}: configured")
        if isinstance(agent, ClaudeDesktopAgent):
            print(f"   {Ui.dim('Restart Claude Desktop to pick up changes.')}")

    return failed


def _print_summary(
    binary_path: Path,
    configured: Iterable[Agent],
    failed: Iterable[Agent] = (),
) -> None:
    configured = list(configured)
    failed = list(failed)
    Ui.rule("Installation complete")
    print(f"  Binary:  {Ui.cyan(str(binary_path))}")
    if configured:
        print(f"  Clients: {', '.join(a.name for a in configured)}")

    if failed:
        print()
        print(f"  {Ui.bold('Manual configuration required (auto-configure failed):')}")
        for a in failed:
            print()
            print(f"  {Ui.bold(a.name)}")
            print(f"   {Ui.dim(a.manual_instructions(binary_path))}")
    print()


def do_check(args: argparse.Namespace) -> int:
    install_dir = Path(args.dir).expanduser().resolve()
    path = resolve_installed_binary(install_dir, platform.system())
    if path:
        print(path)
        return 0
    return 1


def do_uninstall(args: argparse.Namespace) -> int:
    install_dir = Path(args.dir).expanduser().resolve()
    Ui.rule("reCamera Intellisense MCP Server — uninstall")
    removed_any = False

    binary_path = resolve_installed_binary(install_dir, platform.system())
    if binary_path and binary_path.is_file():
        if Ui.ask_yes_no(f"Remove binary {Ui.cyan(str(binary_path))}?", default=True):
            try:
                binary_path.unlink()
                Ui.info(f"Removed {binary_path}")
                removed_any = True
            except OSError as e:
                Ui.warn(f"Could not remove binary: {e}")
    else:
        Ui.info("No installed binary found.")

    agents = all_or_named_agents(args.client)
    for agent in agents:
        try:
            if agent.unconfigure():
                Ui.info(f"{agent.name}: deregistered")
                removed_any = True
        except (OSError, SetupError) as e:
            Ui.warn(f"{agent.name}: {e}")

    print()
    if removed_any:
        Ui.info("Uninstall complete.")
    else:
        Ui.warn("Nothing to uninstall.")
    return 0


def do_configure(args: argparse.Namespace) -> int:
    install_dir = Path(args.dir).expanduser().resolve()
    binary = resolve_installed_binary(install_dir, platform.system())
    if binary is None:
        raise SetupError(
            "No installed binary found; nothing to configure.",
            hint="Run `setup-mcp.py install` first, "
            "or pass --dir to point at an existing binary.",
        )
    Ui.info(f"Using binary: {Ui.cyan(str(binary))}")
    agents = filter_agents(args.client)
    if not agents:
        Ui.warn("No MCP clients selected/detected.")
        return 0
    _configure_agents(agents, binary)
    return 0


def do_list_clients(_: argparse.Namespace) -> int:
    by_key = agents_by_key()
    Ui.rule("Supported MCP clients")
    width = max(len(k) for k in by_key) + 2
    for key, agent in sorted(by_key.items()):
        detected = Ui.green("detected") if agent.detect() else Ui.dim("not detected")
        cfg = str(agent.config_path) if agent.config_path else "(none)"
        print(f"  {Ui.bold(key.ljust(width))}{agent.name:<18} {detected}")
        print(f"  {' ' * width}{Ui.dim(cfg)}")
    return 0


# ══════════════════════════════════════════════════════════════════════
# CLI
# ══════════════════════════════════════════════════════════════════════


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="setup-mcp.py",
        description="Install and configure the reCamera Intellisense MCP server.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  setup-mcp.py install                 (interactive)\n"
            "  setup-mcp.py install -y              (non-interactive)\n"
            "  setup-mcp.py install --client cursor --client vscode\n"
            "  setup-mcp.py install --version v1.2.0\n"
            "  setup-mcp.py check                   (print path; exit 0/1)\n"
            "  setup-mcp.py uninstall -y\n"
        ),
    )
    parser.add_argument(
        "--no-colour",
        action="store_true",
        help="Disable ANSI colour output (same as NO_COLOR=1).",
    )
    # Note: legacy `--check` (no subcommand) is rewritten to `check` by
    # `_normalise_argv` before parsing, so no argparse binding is needed here.

    sub = parser.add_subparsers(dest="command", metavar="<command>")

    known_clients = ", ".join(a().key for a in ALL_AGENT_CLASSES)

    def add_common(p: argparse.ArgumentParser) -> None:
        p.add_argument(
            "--dir",
            default=str(DEFAULT_INSTALL_DIR),
            help=f"Install directory (default: {DEFAULT_INSTALL_DIR}).",
        )
        p.add_argument(
            "--client",
            action="append",
            metavar="NAME",
            help=f"Operate only on the named MCP client. "
            f"May repeat. Known: {known_clients}.",
        )
        p.add_argument(
            "-y",
            "--yes",
            action="store_true",
            help="Non-interactive mode: accept all prompts.",
        )

    install_p = sub.add_parser(
        "install",
        help="Download the binary and configure MCP clients.",
        description="Download the latest (or pinned) release binary "
        "and configure MCP clients.",
    )
    add_common(install_p)
    install_p.add_argument(
        "--version", metavar="TAG", help="Pin a specific release tag (e.g. v1.2.0)."
    )
    install_p.add_argument(
        "--force-download",
        action="store_true",
        help="Re-download even if the binary already exists.",
    )
    install_p.add_argument(
        "--skip-checksum",
        action="store_true",
        help="Bypass SHA-256 verification of the downloaded release asset. "
        "Not recommended — use only on trusted networks or when the release "
        "does not publish a digest yet. Equivalent to RECAMERA_SKIP_CHECKSUM=1.",
    )
    install_p.set_defaults(func=do_install)

    configure_p = sub.add_parser(
        "configure",
        help="Configure MCP clients against an already-installed binary.",
    )
    add_common(configure_p)
    configure_p.set_defaults(func=do_configure)

    check_p = sub.add_parser(
        "check",
        help="Print the installed binary path and exit 0, else exit 1.",
    )
    check_p.add_argument(
        "--dir",
        default=str(DEFAULT_INSTALL_DIR),
        help=f"Install directory (default: {DEFAULT_INSTALL_DIR}).",
    )
    check_p.set_defaults(func=do_check)

    uninstall_p = sub.add_parser(
        "uninstall",
        help="Remove the binary and deregister MCP clients.",
    )
    add_common(uninstall_p)
    uninstall_p.set_defaults(func=do_uninstall)

    list_p = sub.add_parser(
        "list-clients",
        help="List supported MCP clients and detection status.",
    )
    list_p.set_defaults(func=do_list_clients)

    return parser


def _normalise_argv(argv: list[str]) -> list[str]:
    """Translate legacy flags and default to `install` when no command is given."""
    # Legacy `--check` (no subcommand) → `check`
    if "--check" in argv:
        rest = [a for a in argv if a != "--check"]
        return ["check", *rest]

    subcommands = {"install", "configure", "check", "uninstall", "list-clients"}
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
    except SetupError as e:
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
