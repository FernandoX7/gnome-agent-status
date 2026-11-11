#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

echo "==> [1/7] Checking GNOME Shell vs Libadwaita Preferences process boundaries..."
node - <<'NODE'
const fs = require('node:fs');
const path = require('node:path');

const extDir = path.join(process.cwd(), 'extension');
const popoverDir = path.join(extDir, 'popover');

const topJs = fs.readdirSync(extDir)
    .filter(f => f.endsWith('.js'))
    .map(f => path.join(extDir, f));
const popJs = fs.readdirSync(popoverDir)
    .filter(f => f.endsWith('.js'))
    .map(f => path.join(popoverDir, f));

const shellJs = [...topJs.filter(f => path.basename(f) !== 'prefs.js'), ...popJs];
const prefsPath = path.join(extDir, 'prefs.js');
const formatPath = path.join(extDir, 'format.js');

const errors = [];

for (const file of shellJs) {
    const src = fs.readFileSync(file, 'utf8');
    if (/gi:\/\/(?:Gtk|Adw|Gdk)\b|imports\.gi\.(?:Gtk|Adw|Gdk)\b/.test(src)) {
        errors.push(`Shell module imports GTK/Adw/Gdk: ${path.relative(process.cwd(), file)}`);
    }
}

const prefsSrc = fs.readFileSync(prefsPath, 'utf8');
if (/gi:\/\/(?:St|Clutter|Shell|Meta)\b|resource:\/\/\/org\/gnome\/shell\/ui\//i.test(prefsSrc)) {
    errors.push('extension/prefs.js imports GNOME Shell actor/compositor modules (St/Clutter/Main/Shell)');
}

const formatSrc = fs.readFileSync(formatPath, 'utf8');
if (/gi:\/\/|resource:\/\/\/|imports\.gi\b/.test(formatSrc)) {
    errors.push('extension/format.js must remain pure JS with zero GI/resource imports');
}

if (errors.length > 0) {
    for (const err of errors) console.error(`ERROR: ${err}`);
    process.exit(1);
}
NODE

echo "==> [2/7] Checking motion-comfort invariant (zero scale/scale_x/scale_y transforms)..."
node - <<'NODE'
const fs = require('node:fs');
const path = require('node:path');

function stripComments(code) {
    return code
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .replace(/(^|[^:\\])\/\/.*$/gm, '$1');
}

const targets = [
    path.join('extension', 'stylesheet.css'),
    ...fs.readdirSync('extension').filter(f => f.endsWith('.js')).map(f => path.join('extension', f)),
    ...fs.readdirSync(path.join('extension', 'popover')).filter(f => f.endsWith('.js')).map(f => path.join('extension', 'popover', f)),
];

const errors = [];
for (const rel of targets) {
    const clean = stripComments(fs.readFileSync(rel, 'utf8'));
    if (/\b(?:scale_x|scale_y|set_scale)\b|['"]scale-[xy]['"]|\bscale[XY3d]*\s*\(/.test(clean)) {
        errors.push(`Disallowed scale transform in ${rel}`);
    }
}

if (errors.length > 0) {
    for (const err of errors) console.error(`ERROR: ${err}`);
    process.exit(1);
}
NODE

echo "==> [3/7] Checking GNOME 50 ClickGesture invariant..."
node - <<'NODE'
const fs = require('node:fs');
const path = require('node:path');

function stripComments(code) {
    return code
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .replace(/(^|[^:\\])\/\/.*$/gm, '$1');
}

const popFiles = fs.readdirSync(path.join('extension', 'popover'))
    .filter(f => f.endsWith('.js'))
    .map(f => path.join('extension', 'popover', f));
const otherExtFiles = fs.readdirSync('extension')
    .filter(f => f.endsWith('.js') && f !== 'indicator.js')
    .map(f => path.join('extension', f));

const errors = [];
for (const rel of [...popFiles, ...otherExtFiles]) {
    const clean = stripComments(fs.readFileSync(rel, 'utf8'));
    if (/\.connect\s*\(\s*['"]button-(?:press|release)-event['"]/.test(clean)) {
        errors.push(`Legacy button-press/release-event connection in ${rel} (breaks GNOME 50 ClickGesture)`);
    }
}

const indicatorClean = stripComments(fs.readFileSync(path.join('extension', 'indicator.js'), 'utf8'));
if (
    /\.connect\s*\(\s*['"]button-(?:press|release)-event['"]/.test(indicatorClean) &&
    !/Clutter\.ClickGesture/.test(indicatorClean)
) {
    errors.push('extension/indicator.js connects button-press/release-event without primary Clutter.ClickGesture guard');
}

if (errors.length > 0) {
    for (const err of errors) console.error(`ERROR: ${err}`);
    process.exit(1);
}
NODE

echo "==> [4/7] Running node --check across all JavaScript modules..."
JS_FILES=(extension/*.js extension/popover/*.js)
if compgen -G "tests/*.mjs" >/dev/null; then
    JS_FILES+=(tests/*.mjs)
fi
if compgen -G "tests/*.js" >/dev/null; then
    JS_FILES+=(tests/*.js)
fi
for js_file in "${JS_FILES[@]}"; do
    node --check "${js_file}"
done

echo "==> [5/7] Running bash -n across shell scripts..."
SH_FILES=(install.sh uninstall.sh scripts/*.sh)
if compgen -G "tests/*.sh" >/dev/null; then
    SH_FILES+=(tests/*.sh)
fi
for sh_file in "${SH_FILES[@]}"; do
    bash -n "${sh_file}"
done

echo "==> [6/7] Validating GSettings schema with glib-compile-schemas --strict --dry-run..."
glib-compile-schemas --strict --dry-run extension/schemas

echo "==> [7/7] Scanning tracked files for leaked secret tokens..."
node - <<'NODE'
const {execFileSync} = require('node:child_process');
const fs = require('node:fs');

const tracked = execFileSync('git', ['ls-files', '-z'], {encoding: 'utf8'})
    .split('\0')
    .filter(Boolean);

const tokenPatterns = [
    {name: 'Anthropic key/token', re: /\bsk-ant-[A-Za-z0-9_-]{28,}\b/g},
    {name: 'GitHub OAuth/user token', re: /\bgh[ou]_[A-Za-z0-9]{30,}\b/g},
    {name: 'GitHub fine-grained PAT', re: /\bgithub_pat_[A-Za-z0-9_]{36,}\b/g},
    {name: 'Google API key', re: /\bAIzaSy[A-Za-z0-9_-]{33}\b/g},
];

const syntheticHint = /(?:test|mock|fake|dummy|sample|example|redact|placeholder|123456|abcdef|000000|xxxxxx)/i;
const findings = [];

for (const rel of tracked) {
    if (/\.(?:png|jpg|jpeg|gif|webp|ico|woff2?)$/i.test(rel))
        continue;
    if (!fs.existsSync(rel))
        continue;

    const text = fs.readFileSync(rel, 'utf8');
    const isRustOrJsTest =
        rel.startsWith('tests/') ||
        rel.endsWith('_tests.rs') ||
        rel.includes('/tests/');

    const lines = text.split('\n');
    let inRustCfgTest = false;

    for (let idx = 0; idx < lines.length; idx++) {
        const line = lines[idx];
        if (/^\s*#\[\s*cfg\s*\(\s*test\s*\)\s*\]/.test(line))
            inRustCfgTest = true;

        for (const {name, re} of tokenPatterns) {
            re.lastIndex = 0;
            let match;
            while ((match = re.exec(line)) !== null) {
                const token = match[0];
                if ((isRustOrJsTest || inRustCfgTest) && syntheticHint.test(token))
                    continue;
                findings.push(`${rel}:${idx + 1}: potential ${name} matched (${token.slice(0, 10)}…)`);
            }
        }
    }
}

if (findings.length > 0) {
    for (const f of findings) console.error(`SECRET SCAN ERROR: ${f}`);
    process.exit(1);
}
NODE

echo "==> All guardrails passed."
