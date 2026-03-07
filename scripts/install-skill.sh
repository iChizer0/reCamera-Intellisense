#!/bin/bash
set -euo pipefail

REPO_OWNER="iChizer0"
REPO_NAME="reCamera-Intellisense"
REPO_BRANCH="main"
SKILL_NAME="recamera-intellisense"
SKILL_SUBDIR="skills/${SKILL_NAME}"
TARBALL_URL="https://github.com/${REPO_OWNER}/${REPO_NAME}/archive/refs/heads/${REPO_BRANCH}.tar.gz"

info()  { printf '\033[1;34m[info]\033[0m  %s\n' "$*"; }
ok()    { printf '\033[1;32m[ok]\033[0m    %s\n' "$*"; }
warn()  { printf '\033[1;33m[warn]\033[0m  %s\n' "$*"; }
error() { printf '\033[1;31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

check_deps() {
    for cmd in curl tar; do
        command -v "$cmd" >/dev/null 2>&1 || error "'$cmd' is required but not found"
    done
}

detect_claw_dirs() {
    local -a dirs=()
    for d in "$HOME"/.*claw; do
        [[ -d "$d" ]] && dirs+=("$d")
    done
    printf '%s\n' "${dirs[@]+"${dirs[@]}"}"
}

choose_install_path() {
    local -a options=()
    local -a claw_dirs=()
    local recommended_idx=0

    while IFS= read -r d; do
        [[ -n "$d" ]] && claw_dirs+=("$d")
    done < <(detect_claw_dirs)

    local workspace_dir="${PWD}"
    options+=("${workspace_dir}/${SKILL_SUBDIR}")
    info "Detected workspace: ${workspace_dir}"

    for d in "${claw_dirs[@]+"${claw_dirs[@]}"}"; do
        options+=("${d}/${SKILL_SUBDIR}")
        info "Detected claw directory: ${d}"
    done

    options+=("custom")

    echo ""
    echo "Where would you like to install the '${SKILL_NAME}' skill?"
    echo ""
    for i in "${!options[@]}"; do
        local label="${options[$i]}"
        if [[ "$i" -eq "$recommended_idx" ]]; then
            printf '  \033[1;32m[%d] %s  (recommended)\033[0m\n' "$((i + 1))" "$label"
        elif [[ "$label" == "custom" ]]; then
            printf '  [%d] Enter a custom path\n' "$((i + 1))"
        else
            printf '  [%d] %s\n' "$((i + 1))" "$label"
        fi
    done
    echo ""

    local choice
    read -rp "Select an option [1]: " choice
    choice="${choice:-1}"

    if ! [[ "$choice" =~ ^[0-9]+$ ]] || (( choice < 1 || choice > ${#options[@]} )); then
        error "Invalid selection: ${choice}"
    fi

    local selected="${options[$((choice - 1))]}"

    if [[ "$selected" == "custom" ]]; then
        read -rp "Enter the install path: " selected
        [[ -z "$selected" ]] && error "No path provided"
        selected="${selected/#\~/$HOME}"
        if [[ "$selected" != *"/${SKILL_NAME}" ]]; then
            selected="${selected%/}/${SKILL_SUBDIR}"
        fi
    fi

    printf '%s' "$selected"
}

download_and_install() {
    local dest="$1"
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    info "Downloading ${REPO_OWNER}/${REPO_NAME}@${REPO_BRANCH} ..."
    curl -fsSL "$TARBALL_URL" -o "${tmpdir}/repo.tar.gz" \
        || error "Failed to download tarball from ${TARBALL_URL}"

    info "Extracting skill files ..."
    local strip_prefix="${REPO_NAME}-${REPO_BRANCH}/${SKILL_SUBDIR}"
    mkdir -p "$dest"
    tar -xzf "${tmpdir}/repo.tar.gz" \
        -C "$dest" \
        --strip-components=3 \
        "${strip_prefix}" \
        || error "Failed to extract skill from tarball"

    ok "Skill installed to: ${dest}"
}

main() {
    check_deps

    echo ""
    echo "========================================"
    echo "  reCamera Intellisense Skill Installer"
    echo "========================================"
    echo ""

    local dest
    dest="$(choose_install_path)"

    echo ""

    if [[ -d "$dest" ]]; then
        warn "Directory already exists: ${dest}"
        local overwrite
        read -rp "Overwrite? [y/N]: " overwrite
        [[ "$overwrite" =~ ^[Yy]$ ]] || { info "Aborted."; exit 0; }
    fi

    download_and_install "$dest"

    echo ""
    info "You can verify the installation with:"
    echo "  ls -la ${dest}"
    echo ""
}

main "$@"
