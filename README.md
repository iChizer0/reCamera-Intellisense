# reCamera Intellisense

AI-powered monitoring and control for [reCamera Pro](https://wiki.seeedstudio.com/recamera/) — manage devices, configure object detection, capture images/video, and control GPIO, all through a Rust-based [MCP](https://modelcontextprotocol.io/) server that works with any MCP-compatible agent.

## Features

- **Multi-camera management** — register and control multiple reCamera devices from a single host; deploy remotely (PC / Cloud) or locally on the camera (e.g. ZeroClaw).
- **Real-time image & video capture** — on-demand JPG/RAW snapshots and MP4 video recording, independent of the detection pipeline.
- **Intelligent detection monitoring** — configurable AI models, timed schedules, multi-category label filters, region-based rules, and debounce thresholds for precise event triggering.
- **Event correlation & reporting** — the on-device monitor merges rule triggers with captured files in real-time, queryable by time range via HTTP API.
- **Agent-friendly CLI / SDK** — every operation is a single CLI command accepting JSON, or a Python function call, designed for seamless AI-agent integration (Claw, LangChain, etc.).

For detailed API signatures and CLI schemas, see [API Reference](skills/recamera-intellisense/REFERENCE.md).

## Installation

**Note: requires an unreleased reCamera Pro hardware with an experimental firmware, stay tuned for the public release!**

### Option A — ClawHub (recommended)

If you use the [ClawHub](https://clawhub.ai) agent framework, install the skill directly:

```bash
npx clawhub@latest install recamera-intellisense
```

### Option B — Python setup script

```bash
curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-skill.py | python3
```

The installer will prompt you to choose an installation directory (current workspace, detected Claude / Claw roots, detected Nanobot workspaces, or a custom path such as `~/.nanobot`).
