#!/bin/bash
# Build a macOS installer .pkg with a format chooser (CLAP / VST3 / AU).
#
# Usage:  installer/macos/build_pkg.sh <version>
# Inputs (already built by the release workflow):
#   dist/macos/*.clap        — CLAP bundles
#   dist/vst3/*.vst3          — VST3 wrapper bundles
#   dist/au/*.component       — AUv2 wrapper bundles
#   dist/agent/{bin,skills}   — optional: sdsp-bus, sdsp-chain, skills
# Output:
#   dist/SuperDuperDSP-<version>-macos-arm64.pkg
#
# The VST3/AU wrappers load the matching .clap at runtime, so the CLAP
# component is marked required in the chooser and pre-selected.

set -euo pipefail
V="${1:?usage: build_pkg.sh <version>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# Inputs and output live in $D (default dist/; build_mac_release.sh points it at its own folder).
D="${SDSP_PKG_DIST:-dist}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PKGS="$WORK/pkgs"; mkdir -p "$PKGS"

# --- postinstall: strip quarantine so an un-notarised bundle still loads ---
mk_scripts() {
    local dir="$1" glob="$2"
    mkdir -p "$dir"
    cat > "$dir/postinstall" <<EOF
#!/bin/bash
xattr -dr com.apple.quarantine "$glob" 2>/dev/null || true
exit 0
EOF
    chmod +x "$dir/postinstall"
}

# --- one component pkg per format ---------------------------------------
build_component() {
    local id="$1" srcdir="$2" srcglob="$3" dest="$4" out="$5" quarglob="$6"
    [[ -n "$(ls -d $srcdir/$srcglob 2>/dev/null || true)" ]] || { echo "skip $id (no files)"; return 1; }
    local rootd="$WORK/root-$id"
    mkdir -p "$rootd$dest"
    # pkgbuild turns extended attributes (com.apple.provenance, which `xattr -c`
    # cannot remove) into AppleDouble `._<name>` files in the payload — 18 stray
    # `._SuperDuper*.vst3` reached /Library/…/VST3 in the first 0.16.0 build.
    # ditto --noextattr copies without them; COPYFILE_DISABLE keeps pkgbuild off them.
    local f
    for f in $srcdir/$srcglob; do ditto --noextattr --norsrc "$f" "$rootd$dest/$(basename "$f")"; done
    local scriptd="$WORK/scripts-$id"
    mk_scripts "$scriptd" "$quarglob"
    COPYFILE_DISABLE=1 pkgbuild --root "$rootd" --scripts "$scriptd" \
        --identifier "$id" --version "$V" \
        --install-location "/" "$PKGS/$out"
}

HAVE_CLAP=0; HAVE_VST3=0; HAVE_AU=0
build_component co.superduperai.pkg.clap $D/macos "*.clap" \
    "/Library/Audio/Plug-Ins/CLAP" clap.pkg \
    "/Library/Audio/Plug-Ins/CLAP" && HAVE_CLAP=1
build_component co.superduperai.pkg.vst3 $D/vst3 "*.vst3" \
    "/Library/Audio/Plug-Ins/VST3" vst3.pkg \
    "/Library/Audio/Plug-Ins/VST3" && HAVE_VST3=1
build_component co.superduperai.pkg.au $D/au "*.component" \
    "/Library/Audio/Plug-Ins/Components" au.pkg \
    "/Library/Audio/Plug-Ins/Components" && HAVE_AU=1

# --- agent tools: sdsp-bus (MCP gateway) + sdsp-chain + skills -----------
# dist/agent/{bin,skills} → /Library/Application Support/SuperDuperDSP; the
# postinstall runs `sdsp-bus setup` as the logged-in user, which registers the
# MCP server only in agent apps that user already has (and links the skills).
HAVE_AGENT=0
AGENT_HOME="/Library/Application Support/SuperDuperDSP"
if [[ -x $D/agent/bin/sdsp-bus ]]; then
    rootd="$WORK/root-agent"; mkdir -p "$rootd$AGENT_HOME"
    ditto --noextattr --norsrc $D/agent "$rootd$AGENT_HOME"
    scriptd="$WORK/scripts-agent"; mkdir -p "$scriptd"
    cat > "$scriptd/postinstall" <<'EOF2'
#!/bin/bash
A="/Library/Application Support/SuperDuperDSP"
xattr -dr com.apple.quarantine "$A" 2>/dev/null || true
mkdir -p /usr/local/bin
ln -sf "$A/bin/sdsp-bus" /usr/local/bin/sdsp-bus
ln -sf "$A/bin/sdsp-chain" /usr/local/bin/sdsp-chain
U="$(stat -f%Su /dev/console)"
if [[ -n "$U" && "$U" != root && "$U" != loginwindow ]]; then
    sudo -u "$U" -H "$A/bin/sdsp-bus" setup > "/tmp/superduper-agent-setup.log" 2>&1 || true
fi
exit 0
EOF2
    chmod +x "$scriptd/postinstall"
    COPYFILE_DISABLE=1 pkgbuild --root "$rootd" --scripts "$scriptd" \
        --identifier co.superduperai.pkg.agent --version "$V" \
        --install-location "/" "$PKGS/agent.pkg" && HAVE_AGENT=1
fi

# --- distribution: chooser UI -------------------------------------------
DIST="$WORK/distribution.xml"
{
    echo '<?xml version="1.0" encoding="utf-8"?>'
    echo '<installer-gui-script minSpecVersion="2">'
    echo '  <title>SuperDuper DSP</title>'
    echo '  <organization>co.superduperai</organization>'
    echo '  <options customize="always" require-scripts="false" hostArchitectures="arm64,x86_64"/>'
    echo '  <choices-outline>'
    [[ $HAVE_CLAP == 1 ]] && echo '    <line choice="clap"/>'
    [[ $HAVE_VST3 == 1 ]] && echo '    <line choice="vst3"/>'
    [[ $HAVE_AU   == 1 ]] && echo '    <line choice="au"/>'
    [[ $HAVE_AGENT == 1 ]] && echo '    <line choice="agent"/>'
    echo '  </choices-outline>'
    # CLAP required (wrappers depend on it); pre-selected + not unselectable.
    [[ $HAVE_CLAP == 1 ]] && cat <<XML
  <choice id="clap" title="CLAP (REAPER, Bitwig)" enabled="false" selected="true">
    <pkg-ref id="co.superduperai.pkg.clap"/>
  </choice>
XML
    [[ $HAVE_VST3 == 1 ]] && cat <<XML
  <choice id="vst3" title="VST3 (Cubase, Ableton, FL, DaVinci)" start_selected="true">
    <pkg-ref id="co.superduperai.pkg.vst3"/>
  </choice>
XML
    [[ $HAVE_AU == 1 ]] && cat <<XML
  <choice id="au" title="Audio Unit (Logic Pro)" start_selected="true">
    <pkg-ref id="co.superduperai.pkg.au"/>
  </choice>
XML
    [[ $HAVE_AGENT == 1 ]] && cat <<XML
  <choice id="agent" title="Agent tools (MCP for Claude, Cursor, Codex)" start_selected="true"
          description="sdsp-bus and sdsp-chain command-line tools, mixing skills, and the superduper-bus MCP server registered in the agent apps you already have. Lets an AI assistant hear and set every SuperDuper plugin. Undo: sdsp-bus setup --uninstall">
    <pkg-ref id="co.superduperai.pkg.agent"/>
  </choice>
XML
    [[ $HAVE_AGENT == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.agent\" version=\"$V\">agent.pkg</pkg-ref>"
    [[ $HAVE_CLAP == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.clap\" version=\"$V\">clap.pkg</pkg-ref>"
    [[ $HAVE_VST3 == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.vst3\" version=\"$V\">vst3.pkg</pkg-ref>"
    [[ $HAVE_AU   == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.au\" version=\"$V\">au.pkg</pkg-ref>"
    echo '</installer-gui-script>'
} > "$DIST"

mkdir -p "$D"
productbuild --distribution "$DIST" --package-path "$PKGS" \
    "$D/SuperDuperDSP-${V}-macos-arm64.pkg"
echo "==> $D/SuperDuperDSP-${V}-macos-arm64.pkg"
