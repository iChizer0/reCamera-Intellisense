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
import copy
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
from urllib.parse import quote
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
MAX_DOWNLOAD_BYTES = 200 * 1024 * 1024
NETWORK_HINT = (
    "Check your internet connection and proxy environment variables "
    "(HTTP_PROXY, HTTPS_PROXY, NO_PROXY)."
)

KNOWN_SUBCOMMANDS: set[str] = set()


# ══════════════════════════════════════════════════════════════════════
# Presentation layer — colours, prompts, errors
# ══════════════════════════════════════════════════════════════════════


def _use_colour() -> bool:
    if os.environ.get("NO_COLOR"):
        return False
    if os.environ.get("FORCE_COLOR"):
        return True
    return sys.stdout.isatty()


_COLOUR = _use_colour()


class Ui:
    """Tiny ANSI-only presentation helper. All output goes through here."""

    auto_yes = False  # populated from CLI args

    @classmethod
    def _wrap(cls, code: str, text: str) -> str:
        return f"\033[{code}m{text}\033[0m" if _COLOUR else text

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
        print(f" {cls.yellow('!')} {msg}", file=sys.stderr)

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
            hint=(
                f"Supported targets:\n    {supported}\n\n"
                "CLI fallback (Python transport):\n"
                "    PYTHONPATH=scripts python3 -m recamera_intellisense "
                "list-commands\n"
                "If you need this target, please open an issue in the "
                "repository."
            ),
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
            hint=NETWORK_HINT,
        ) from None


def fetch_release(version: str | None) -> dict:
    if version:
        Ui.step(f"Fetching release {Ui.bold(version)} from GitHub…")
        encoded_version = quote(version, safe="")
        return _github_get(
            f"https://api.github.com/repos/{REPO}/releases/tags/{encoded_version}",
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
        request = Request(url, headers={"User-Agent": f"setup-mcp.py/{REPO}"})
        with urlopen(request, timeout=HTTP_TIMEOUT_DOWNLOAD) as resp:
            total_hdr = resp.headers.get("Content-Length")
            total = int(total_hdr) if total_hdr else 0
            if total > MAX_DOWNLOAD_BYTES:
                raise SetupError(
                    "Release asset is too large to download safely.",
                    hint=(
                        f"Asset size {_format_mb(total)} exceeds limit "
                        f"{_format_mb(MAX_DOWNLOAD_BYTES)}."
                    ),
                )
            received = 0
            last_pct = -1
            with open(dest, "wb") as f:
                while True:
                    chunk = resp.read(CHUNK_SIZE)
                    if not chunk:
                        break
                    f.write(chunk)
                    received += len(chunk)
                    if received > MAX_DOWNLOAD_BYTES:
                        raise SetupError(
                            "Release asset exceeded maximum download size.",
                            hint=(
                                f"Received {_format_mb(received)} which exceeds "
                                f"limit {_format_mb(MAX_DOWNLOAD_BYTES)}."
                            ),
                        )
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
    except SetupError:
        if dest.exists():
            try:
                dest.unlink()
            except OSError:
                pass
        raise
    except (HTTPError, URLError) as e:
        raise SetupError(f"Download failed: {e}", hint=NETWORK_HINT) from None


def _fetch_text(
    url: str, *, timeout: int = HTTP_TIMEOUT_API, max_bytes: int = 1 << 20
) -> str:
    """Download a small text asset (checksums file). Hard cap to prevent
    unexpectedly huge payloads from exhausting memory."""
    try:
        with urlopen(Request(url), timeout=timeout) as resp:
            data = resp.read(max_bytes + 1)
    except (HTTPError, URLError) as e:
        raise SetupError(
            f"Failed to download checksum asset: {e}",
            hint=NETWORK_HINT,
        ) from None
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
    first_token = tokens[0].lstrip("\ufeff") if tokens else ""
    if len(tokens) == 1 and len(first_token) == 64:
        try:
            int(first_token, 16)
            return first_token.lower()
        except ValueError:
            pass
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) < 2:
            continue
        digest = parts[0].lstrip("\ufeff").lstrip("*").lower()
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
        # Dual traversal guard: reject absolute/.. prefixes AND any embedded
        # parent-segment in normalized path parts.
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
        # Dual traversal guard: reject absolute/.. prefixes AND any embedded
        # parent-segment in normalized path parts.
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

    def post_configure_hint(self, binary_path: Path) -> str | None:
        return None

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

    @staticmethod
    def _canonical_json(value: object) -> str:
        return json.dumps(value, sort_keys=True, separators=(",", ":"))

    def _backup_path(self) -> Path:
        assert self.config_path is not None
        return self.config_path.parent / ".recamera.bak"

    def _legacy_backup_path(self) -> Path:
        assert self.config_path is not None
        return self.config_path.with_suffix(self.config_path.suffix + ".bak")

    def _delete_config_and_backups(self) -> None:
        assert self.config_path is not None
        if self.config_path.exists():
            self.config_path.unlink()
        for backup in (self._backup_path(), self._legacy_backup_path()):
            if backup.exists():
                backup.unlink()

    def _contains_only_recamera_registration(self, cfg: dict) -> bool:
        probe = copy.deepcopy(cfg)
        if self.is_registered(probe):
            self.deregister(probe)
        return probe == {}

    def write_config(self, data: dict) -> None:
        assert self.config_path is not None
        self.config_path.parent.mkdir(parents=True, exist_ok=True)

        new_text = json.dumps(data, indent=2) + "\n"
        new_canonical = self._canonical_json(data)

        if self.config_path.exists():
            existing_text = self.config_path.read_text(encoding="utf-8")
            try:
                existing_obj = (
                    json.loads(existing_text) if existing_text.strip() else {}
                )
            except json.JSONDecodeError:
                existing_obj = None

            if existing_obj is not None:
                existing_canonical = self._canonical_json(existing_obj)
                if existing_canonical == new_canonical:
                    return

            shutil.copy2(self.config_path, self._backup_path())

        tmp = self.config_path.with_suffix(self.config_path.suffix + ".tmp")
        tmp.write_text(new_text, encoding="utf-8")
        os.replace(tmp, self.config_path)

    def configure_with(self, cfg: dict, binary_path: Path) -> None:
        self.apply(cfg, binary_path)
        self.write_config(cfg)

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        if cfg is None:
            raise SetupError(
                f"{self.name} config is not valid JSON: {self.config_path}",
                hint="Fix or remove the file by hand before re-running — "
                "I refuse to overwrite user data.",
            )
        self.configure_with(cfg, binary_path)

    def unconfigure(self) -> bool:
        cfg = self.read_config()
        if cfg is None or not cfg:
            return False
        if not self.is_registered(cfg):
            return False

        had_only_recamera = self._contains_only_recamera_registration(cfg)
        self.deregister(cfg)
        if cfg == {} and had_only_recamera:
            self._delete_config_and_backups()
        else:
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
        # VS Code stores MCP entries under `mcp.servers.<name>`.
        cfg.setdefault("mcp", {}).setdefault("servers", {})[SERVER_KEY] = {
            "command": str(binary_path),
        }

    def post_configure_hint(self, binary_path: Path) -> str | None:
        return "Reload VS Code Window to pick up MCP changes."

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
            # No official Linux Claude Desktop build today, but community
            # setups and compatibility layers may still use this config path.
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
        # Claude Desktop expects `mcpServers.<name> = {"command": ...}`.
        cfg.setdefault("mcpServers", {})[SERVER_KEY] = {"command": str(binary_path)}

    def post_configure_hint(self, binary_path: Path) -> str | None:
        return "Restart Claude Desktop to pick up changes."

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
        return shutil.which("claude") is not None or self.config_exists()

    def apply(self, cfg: dict, binary_path: Path) -> None:
        # Claude Code MCP schema requires explicit stdio transport shape.
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
        # Cursor uses Claude-style `mcpServers.<name> = {"command": ...}`.
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
        # Windsurf uses Claude-style `mcpServers.<name> = {"command": ...}`.
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
        # Nanobot nests MCP servers under `tools.mcpServers.<name>`.
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
    instances = [cls() for cls in ALL_AGENT_CLASSES]
    return {a.key: a for a in instances}


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
    instances = [cls() for cls in ALL_AGENT_CLASSES]
    return [agent for agent in instances if agent.detect()]


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


@dataclass
class ConfigureReport:
    configured: list[Agent]
    already: list[Agent]
    skipped: list[Agent]
    failed: list[Agent]


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
    report = ConfigureReport(configured=[], already=[], skipped=[], failed=[])
    if not agents and not args.client:
        Ui.warn("No supported MCP clients detected on this system.")
    else:
        report = _configure_agents(agents, binary_path)

    _print_summary(binary_path, report)

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


def _configure_agents(agents: Iterable[Agent], binary_path: Path) -> ConfigureReport:
    """Run interactive configuration. Returns the subset whose auto-configure
    failed (broken config, write error, or raised SetupError) — these are
    the only agents we still owe a manual-setup snippet at the end."""
    agents = list(agents)
    report = ConfigureReport(configured=[], already=[], skipped=[], failed=[])
    if not agents:
        return report

    print()
    Ui.info(f"MCP clients: {', '.join(Ui.bold(a.name) for a in agents)}")

    for agent in agents:
        cfg = agent.read_config()
        if cfg is None:
            Ui.warn(
                f"{agent.name}: config is not valid JSON "
                f"({agent.config_path}). Skipping — refusing to overwrite."
            )
            Ui.hint("Fix the file by hand then re-run `configure`.")
            report.failed.append(agent)
            continue

        if agent.is_registered(cfg):
            Ui.info(f"{agent.name}: already configured — skipping")
            report.already.append(agent)
            continue

        prompt = f"Configure {Ui.bold(agent.name)} ({Ui.dim(str(agent.config_path))})?"
        if not Ui.ask_yes_no(prompt, default=True):
            Ui.info(f"{agent.name}: skipped")
            report.skipped.append(agent)
            continue

        try:
            agent.configure_with(cfg, binary_path)
        except SetupError as e:
            Ui.warn(f"{agent.name}: {e}")
            if e.hint:
                Ui.hint(e.hint)
            report.failed.append(agent)
            continue
        except OSError as e:
            Ui.warn(f"{agent.name}: failed to write config — {e}")
            report.failed.append(agent)
            continue

        Ui.info(f"{agent.name}: configured")
        report.configured.append(agent)
        hint = agent.post_configure_hint(binary_path)
        if hint:
            print(f"   {Ui.dim(hint)}")

    return report


def _print_summary(
    binary_path: Path,
    report: ConfigureReport,
) -> None:
    configured = list(report.configured)
    already = list(report.already)
    skipped = list(report.skipped)
    failed = list(report.failed)

    Ui.rule("Installation complete")
    print(f"  Binary:  {Ui.cyan(str(binary_path))}")
    if configured:
        print(f"  Clients: {', '.join(a.name for a in configured)}")
    if already:
        print(f"  Already: {', '.join(a.name for a in already)}")
    if skipped:
        print(f"  Skipped: {', '.join(a.name for a in skipped)}")

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

    if not sys.stdin.isatty() and not bool(getattr(args, "yes", False)):
        raise SetupError(
            "Refusing to run uninstall non-interactively without --yes.",
            hint="Re-run with `uninstall -y` to confirm removal.",
        )

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

    def add_common(
        p: argparse.ArgumentParser,
        *,
        include_client: bool = True,
        include_yes: bool = True,
    ) -> None:
        p.add_argument(
            "--dir",
            default=str(DEFAULT_INSTALL_DIR),
            help=f"Install directory (default: {DEFAULT_INSTALL_DIR}).",
        )
        if include_client:
            p.add_argument(
                "--client",
                action="append",
                metavar="NAME",
                help=f"Operate only on the named MCP client. "
                f"May repeat. Known: {known_clients}.",
            )
        if include_yes:
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
    add_common(check_p, include_client=False, include_yes=False)
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

    global KNOWN_SUBCOMMANDS
    KNOWN_SUBCOMMANDS = set(sub.choices.keys())

    return parser


def _normalise_argv(argv: list[str], subcommands: set[str]) -> list[str]:
    """Translate legacy flags and default to `install` when no command is given."""
    # Legacy `--check` (no subcommand) → `check`
    if "--check" in argv:
        rest = [a for a in argv if a != "--check"]
        return ["check", *rest]

    if any(a in ("-h", "--help") for a in argv):
        return argv

    value_flags = {"--dir", "--client", "--version"}
    positionals: list[str] = []
    i = 0
    while i < len(argv):
        token = argv[i]
        if token == "--":
            positionals.extend(a for a in argv[i + 1 :] if not a.startswith("-"))
            break
        if token in value_flags:
            i += 2
            continue
        if any(token.startswith(f"{flag}=") for flag in value_flags):
            i += 1
            continue
        if not token.startswith("-"):
            positionals.append(token)
        i += 1

    first_positional = positionals[0] if positionals else None
    if first_positional not in subcommands:
        return ["install", *argv]
    return argv


def main(argv: list[str] | None = None) -> int:
    global _COLOUR

    argv = list(sys.argv[1:] if argv is None else argv)

    def _sigint(_signum, _frame):
        print()
        Ui.warn("Interrupted.")
        sys.exit(130)

    signal.signal(signal.SIGINT, _sigint)

    parser = _build_parser()
    argv = _normalise_argv(argv, KNOWN_SUBCOMMANDS)
    args = parser.parse_args(argv)

    if getattr(args, "no_colour", False):
        _COLOUR = False

    Ui.auto_yes = bool(getattr(args, "yes", False)) or not sys.stdin.isatty()

    # Defensive branch: `_normalise_argv` normally guarantees a command,
    # but keeping this guard avoids silent breakage if parser wiring changes.
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
