#!/usr/bin/env python3
"""
reCamera Intellisense MCP Server — Setup Script

Auto-detects platform, downloads the latest release binary from GitHub,
installs it to ~/.recamera/bin, and optionally configures detected MCP clients.

Usage:
    python3 setup-mcp.py             # interactive
    python3 setup-mcp.py --yes       # non-interactive (auto-configure all detected clients)
    python3 setup-mcp.py --check     # print binary path if installed, exit 0/1
    curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py | python3
"""

import argparse
import json
import os
import platform
import shutil
import stat
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

REPO = "iChizer0/reCamera-Intellisense"
BINARY_NAME = "recamera-intellisense-mcp"
INSTALL_DIR = Path.home() / ".recamera" / "bin"
GITHUB_API = f"https://api.github.com/repos/{REPO}/releases/latest"

# Maps (system, machine) to the GitHub release asset name stem.
# system = platform.system(), machine = platform.machine()
PLATFORM_MAP = {
    ("Linux", "x86_64"): "recamera-intellisense-mcp-linux-x86_64",
    ("Linux", "aarch64"): "recamera-intellisense-mcp-linux-aarch64",
    ("Darwin", "arm64"): "recamera-intellisense-mcp-macos-aarch64",
    ("Windows", "AMD64"): "recamera-intellisense-mcp-windows-x86_64",
}

# ── Colour helpers (disabled when not a TTY) ──────────────────────────

_colour = sys.stdout.isatty()


def _fmt(code: str, text: str) -> str:
    return f"\033[{code}m{text}\033[0m" if _colour else text


def _bold(text: str) -> str:
    return _fmt("1", text)


def _green(text: str) -> str:
    return _fmt("32", text)


def _yellow(text: str) -> str:
    return _fmt("33", text)


def _red(text: str) -> str:
    return _fmt("1;31", text)


def _cyan(text: str) -> str:
    return _fmt("36", text)


def _dim(text: str) -> str:
    return _fmt("2", text)


# ── Utilities ─────────────────────────────────────────────────────────


def info(msg: str) -> None:
    print(f"{_green('✓')} {msg}")


def warn(msg: str) -> None:
    print(f"{_yellow('!')} {msg}")


def error(msg: str) -> None:
    print(f"{_red('✗')} {msg}", file=sys.stderr)


# Set by CLI args; when True, all prompts auto-accept.
_auto_yes = False


def ask_yes_no(prompt: str, default: bool = True) -> bool:
    if _auto_yes or not sys.stdin.isatty():
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


# ── Platform detection ────────────────────────────────────────────────


def detect_platform() -> tuple[str, str]:
    """Return (asset_stem, extension) for this platform."""
    system = platform.system()
    machine = platform.machine()

    key = (system, machine)
    asset_stem = PLATFORM_MAP.get(key)
    if asset_stem is None:
        error(f"Unsupported platform: {system} {machine}")
        print(f"  Supported platforms:")
        for (s, m), name in PLATFORM_MAP.items():
            print(f"    {s} {m}  →  {name}")
        sys.exit(1)

    ext = ".zip" if system == "Windows" else ".tar.gz"
    info(f"Detected platform: {_bold(system)} {_bold(machine)}")
    return asset_stem, ext


# ── GitHub release fetching ───────────────────────────────────────────


def fetch_latest_release() -> dict:
    """Fetch latest release metadata from GitHub API."""
    info("Fetching latest release info from GitHub...")
    req = Request(GITHUB_API, headers={"Accept": "application/vnd.github+json"})
    try:
        with urlopen(req, timeout=30) as resp:
            return json.loads(resp.read())
    except HTTPError as e:
        if e.code == 404:
            error(
                "No releases found. The repository may not have published a release yet."
            )
        else:
            error(f"GitHub API error: HTTP {e.code}")
        sys.exit(1)
    except URLError as e:
        error(f"Network error: {e.reason}")
        sys.exit(1)


def find_asset(release: dict, asset_stem: str, ext: str) -> tuple[str, str]:
    """Find the matching asset in a release. Returns (download_url, filename)."""
    target_name = asset_stem + ext
    for asset in release.get("assets", []):
        if asset["name"] == target_name:
            return asset["browser_download_url"], asset["name"]

    error(f"Release {release.get('tag_name', '?')} has no asset named '{target_name}'")
    available = [a["name"] for a in release.get("assets", [])]
    if available:
        print(f"  Available assets: {', '.join(available)}")
    sys.exit(1)


# ── Download and install ──────────────────────────────────────────────


def download_file(url: str, dest: Path) -> None:
    """Download a URL to a local file with progress indication."""
    req = Request(url)
    show_progress = sys.stdout.isatty() and not _auto_yes
    try:
        with urlopen(req, timeout=120) as resp:
            total = resp.headers.get("Content-Length")
            total = int(total) if total else None
            downloaded = 0
            with open(dest, "wb") as f:
                while True:
                    chunk = resp.read(65536)
                    if not chunk:
                        break
                    f.write(chunk)
                    downloaded += len(chunk)
                    if total and show_progress:
                        pct = downloaded * 100 // total
                        mb = downloaded / (1024 * 1024)
                        print(
                            f"\r  Downloading... {mb:.1f} MB ({pct}%)",
                            end="",
                            flush=True,
                        )
            if total and show_progress:
                print()
    except (HTTPError, URLError) as e:
        error(f"Download failed: {e}")
        sys.exit(1)


def extract_and_install(archive_path: Path, filename: str) -> Path:
    """Extract the binary from the archive and place it in INSTALL_DIR."""
    INSTALL_DIR.mkdir(parents=True, exist_ok=True)

    is_windows = platform.system() == "Windows"
    binary = BINARY_NAME + (".exe" if is_windows else "")
    dest = INSTALL_DIR / binary

    with tempfile.TemporaryDirectory() as tmpdir:
        tmpdir = Path(tmpdir)
        if filename.endswith(".tar.gz"):
            with tarfile.open(archive_path, "r:gz") as tar:
                # Security: only extract expected binary, avoid path traversal
                members = [
                    m
                    for m in tar.getmembers()
                    if Path(m.name).name == binary
                    and not m.name.startswith(("/", ".."))
                ]
                if not members:
                    error(f"Binary '{binary}' not found in archive")
                    sys.exit(1)
                tar.extract(members[0], path=tmpdir)
                extracted = tmpdir / members[0].name
        elif filename.endswith(".zip"):
            with zipfile.ZipFile(archive_path, "r") as zf:
                names = [
                    n
                    for n in zf.namelist()
                    if Path(n).name == binary and not n.startswith(("/", ".."))
                ]
                if not names:
                    error(f"Binary '{binary}' not found in archive")
                    sys.exit(1)
                zf.extract(names[0], path=tmpdir)
                extracted = tmpdir / names[0]
        else:
            error(f"Unknown archive format: {filename}")
            sys.exit(1)

        shutil.move(str(extracted), str(dest))

    if not is_windows:
        dest.chmod(dest.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    return dest


# ── Agent detection and configuration ─────────────────────────────────


class Agent:
    """Base class for MCP client agents."""

    name: str
    config_path: Path | None

    def detect(self) -> bool:
        raise NotImplementedError

    def config_exists(self) -> bool:
        return self.config_path is not None and self.config_path.exists()

    def read_config(self) -> dict:
        if self.config_path and self.config_path.exists():
            try:
                return json.loads(self.config_path.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, OSError):
                return {}
        return {}

    def write_config(self, data: dict) -> None:
        if self.config_path:
            self.config_path.parent.mkdir(parents=True, exist_ok=True)
            self.config_path.write_text(
                json.dumps(data, indent=2) + "\n", encoding="utf-8"
            )

    def is_already_configured(self) -> bool:
        raise NotImplementedError

    def configure(self, binary_path: Path) -> None:
        raise NotImplementedError

    def manual_instructions(self, binary_path: Path) -> str:
        raise NotImplementedError


class VSCode(Agent):
    name = "VS Code"

    def __init__(self) -> None:
        system = platform.system()
        if system == "Darwin":
            self.config_path = (
                Path.home()
                / "Library"
                / "Application Support"
                / "Code"
                / "User"
                / "settings.json"
            )
        elif system == "Windows":
            self.config_path = (
                Path(os.environ.get("APPDATA", "")) / "Code" / "User" / "settings.json"
            )
        else:
            self.config_path = (
                Path.home() / ".config" / "Code" / "User" / "settings.json"
            )

    def detect(self) -> bool:
        return shutil.which("code") is not None or self.config_exists()

    def is_already_configured(self) -> bool:
        cfg = self.read_config()
        return "recamera" in cfg.get("mcp", {}).get("servers", {})

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        cfg.setdefault("mcp", {}).setdefault("servers", {})["recamera"] = {
            "command": str(binary_path),
        }
        self.write_config(cfg)

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"  Add to settings.json or .vscode/mcp.json:\n"
            f'  {{"mcp": {{"servers": {{"recamera": {{"command": "{binary_path}"}}}}}}}}'
        )


class ClaudeDesktop(Agent):
    name = "Claude Desktop"

    def __init__(self) -> None:
        system = platform.system()
        if system == "Darwin":
            self.config_path = (
                Path.home()
                / "Library"
                / "Application Support"
                / "Claude"
                / "claude_desktop_config.json"
            )
        elif system == "Windows":
            self.config_path = (
                Path(os.environ.get("APPDATA", ""))
                / "Claude"
                / "claude_desktop_config.json"
            )
        else:
            self.config_path = (
                Path.home() / ".config" / "Claude" / "claude_desktop_config.json"
            )

    def detect(self) -> bool:
        if shutil.which("claude-desktop") is not None:
            return True
        if platform.system() == "Darwin":
            return Path("/Applications/Claude.app").exists()
        return self.config_exists()

    def is_already_configured(self) -> bool:
        cfg = self.read_config()
        return "recamera" in cfg.get("mcpServers", {})

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        cfg.setdefault("mcpServers", {})["recamera"] = {
            "command": str(binary_path),
        }
        self.write_config(cfg)

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"  Add to claude_desktop_config.json:\n"
            f'  {{"mcpServers": {{"recamera": {{"command": "{binary_path}"}}}}}}'
        )


class ClaudeCode(Agent):
    name = "Claude Code"

    def __init__(self) -> None:
        self.config_path = Path.home() / ".claude.json"

    def detect(self) -> bool:
        return shutil.which("claude") is not None

    def is_already_configured(self) -> bool:
        cfg = self.read_config()
        return "recamera" in cfg.get("mcpServers", {})

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        cfg.setdefault("mcpServers", {})["recamera"] = {
            "type": "stdio",
            "command": str(binary_path),
            "args": [],
        }
        self.write_config(cfg)

    def manual_instructions(self, binary_path: Path) -> str:
        return f"  Run: claude mcp add --transport stdio recamera -- {binary_path}"


class Cursor(Agent):
    name = "Cursor"

    def __init__(self) -> None:
        self.config_path = Path.home() / ".cursor" / "mcp.json"

    def detect(self) -> bool:
        if shutil.which("cursor") is not None:
            return True
        if platform.system() == "Darwin":
            return Path("/Applications/Cursor.app").exists()
        return self.config_exists()

    def is_already_configured(self) -> bool:
        cfg = self.read_config()
        return "recamera" in cfg.get("mcpServers", {})

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        cfg.setdefault("mcpServers", {})["recamera"] = {
            "command": str(binary_path),
        }
        self.write_config(cfg)

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"  Add to ~/.cursor/mcp.json:\n"
            f'  {{"mcpServers": {{"recamera": {{"command": "{binary_path}"}}}}}}'
        )


class Windsurf(Agent):
    name = "Windsurf"

    def __init__(self) -> None:
        system = platform.system()
        if system == "Windows":
            self.config_path = (
                Path(os.environ.get("APPDATA", ""))
                / "Codeium"
                / "Windsurf"
                / "mcp_config.json"
            )
        else:
            self.config_path = Path.home() / ".codeium" / "windsurf" / "mcp_config.json"

    def detect(self) -> bool:
        if shutil.which("windsurf") is not None:
            return True
        if platform.system() == "Darwin":
            return Path("/Applications/Windsurf.app").exists()
        return self.config_exists()

    def is_already_configured(self) -> bool:
        cfg = self.read_config()
        return "recamera" in cfg.get("mcpServers", {})

    def configure(self, binary_path: Path) -> None:
        cfg = self.read_config()
        cfg.setdefault("mcpServers", {})["recamera"] = {
            "command": str(binary_path),
        }
        self.write_config(cfg)

    def manual_instructions(self, binary_path: Path) -> str:
        return (
            f"  Add to mcp_config.json:\n"
            f'  {{"mcpServers": {{"recamera": {{"command": "{binary_path}"}}}}}}'
        )


ALL_AGENTS: list[type[Agent]] = [VSCode, ClaudeDesktop, ClaudeCode, Cursor, Windsurf]


def detect_agents() -> list[Agent]:
    found = []
    for cls in ALL_AGENTS:
        agent = cls()
        if agent.detect():
            found.append(agent)
    return found


def configure_agents(agents: list[Agent], binary_path: Path) -> None:
    if not agents:
        return

    print()
    info(f"Detected MCP clients: {', '.join(_bold(a.name) for a in agents)}")

    for agent in agents:
        if agent.is_already_configured():
            info(f"  {agent.name}: already configured — skipping")
            continue

        if ask_yes_no(f"  Configure {_bold(agent.name)} to use the MCP server?"):
            try:
                agent.configure(binary_path)
                info(f"  {agent.name}: configured ✓")
                if isinstance(agent, ClaudeDesktop):
                    print(f"    {_dim('Restart Claude Desktop to pick up changes.')}")
            except OSError as e:
                warn(f"  {agent.name}: failed to write config — {e}")
        else:
            print(f"    Skipped. Manual setup:")
            print(agent.manual_instructions(binary_path))


# ── Summary ───────────────────────────────────────────────────────────


def print_summary(binary_path: Path, agents: list[Agent]) -> None:
    print()
    print(_bold("─" * 60))
    print(_bold("  Installation complete"))
    print(_bold("─" * 60))
    print()
    print(f"  Binary:  {_cyan(str(binary_path))}")
    print()
    print(_bold("  Manual configuration for other clients:"))
    for agent_cls in ALL_AGENTS:
        agent = agent_cls()
        print()
        print(f"  {_bold(agent.name)}:")
        print(agent.manual_instructions(binary_path))
    print()
    print()


# ── Check mode ────────────────────────────────────────────────────────


def check_installed() -> None:
    """Print binary path and exit 0 if installed, else exit 1."""
    is_windows = platform.system() == "Windows"
    binary = BINARY_NAME + (".exe" if is_windows else "")
    path = INSTALL_DIR / binary
    if path.is_file():
        print(str(path))
        sys.exit(0)
    # Also check PATH
    which = shutil.which(BINARY_NAME)
    if which:
        print(which)
        sys.exit(0)
    sys.exit(1)


# ── Main ──────────────────────────────────────────────────────────────


def main() -> None:
    global _auto_yes

    parser = argparse.ArgumentParser(
        description="Install the reCamera Intellisense MCP server binary.",
    )
    parser.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="Non-interactive mode: auto-configure all detected MCP clients.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check if already installed. Prints binary path and exits 0, or exits 1.",
    )
    args = parser.parse_args()

    if args.check:
        check_installed()
        return

    _auto_yes = args.yes or not sys.stdin.isatty()

    print()
    print(_bold("  reCamera Intellisense MCP Server — Setup"))
    print()

    # 1. Detect platform
    asset_stem, ext = detect_platform()

    # 2. Fetch latest release
    release = fetch_latest_release()
    tag = release.get("tag_name", "unknown")
    info(f"Latest release: {_bold(tag)}")

    # 3. Find matching asset
    download_url, filename = find_asset(release, asset_stem, ext)

    # 4. Download
    with tempfile.TemporaryDirectory() as tmpdir:
        archive_path = Path(tmpdir) / filename
        download_file(download_url, archive_path)
        info(f"Downloaded {filename}")

        # 5. Extract and install
        binary_path = extract_and_install(archive_path, filename)
        info(f"Installed to {_cyan(str(binary_path))}")

    # 6. Detect agents and offer to configure
    agents = detect_agents()
    configure_agents(agents, binary_path)

    # 7. Print summary
    print_summary(binary_path, agents)

    # 8. Machine-readable final line for agents
    print(f"BINARY_PATH={binary_path}")


if __name__ == "__main__":
    main()
