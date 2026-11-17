import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {describe, it} from 'node:test';
import {fileURLToPath} from 'node:url';

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(TEST_DIR, '..');
const SELECT_SCRIPT = path.join(REPO_ROOT, 'scripts', 'ci-select.sh');

function parseOutputs(stdout) {
    const out = {};
    for (const line of stdout.trim().split('\n')) {
        if (!line)
            continue;
        const eq = line.indexOf('=');
        if (eq > 0)
            out[line.slice(0, eq)] = line.slice(eq + 1);
    }
    return out;
}

function initTempGitRepo() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'gas-ci-select-'));
    execFileSync('git', ['init', '-q'], {cwd: dir});
    execFileSync('git', ['config', 'user.email', 'ci@example.com'], {cwd: dir});
    execFileSync('git', ['config', 'user.name', 'CI Test'], {cwd: dir});
    fs.writeFileSync(path.join(dir, 'README.md'), '# init\n');
    execFileSync('git', ['add', '.'], {cwd: dir});
    execFileSync('git', ['commit', '-q', '-m', 'initial'], {cwd: dir});
    const baseSha = execFileSync('git', ['rev-parse', 'HEAD'], {cwd: dir, encoding: 'utf8'}).trim();
    return {dir, baseSha};
}

function commitFiles(dir, files, msg = 'change') {
    for (const [rel, content] of Object.entries(files)) {
        const full = path.join(dir, rel);
        fs.mkdirSync(path.dirname(full), {recursive: true});
        fs.writeFileSync(full, content);
    }
    execFileSync('git', ['add', '.'], {cwd: dir});
    execFileSync('git', ['commit', '-q', '-m', msg], {cwd: dir});
    return execFileSync('git', ['rev-parse', 'HEAD'], {cwd: dir, encoding: 'utf8'}).trim();
}

function runSelect(dir, env) {
    return spawnSync('bash', [SELECT_SCRIPT], {
        cwd: dir,
        env: {...process.env, GITHUB_OUTPUT: '', ...env},
        encoding: 'utf8',
    });
}

describe('CI diff reachability selection (scripts/ci-select.sh)', () => {
    it('runs the full suite unconditionally on push and merge_group', () => {
        const {dir, baseSha} = initTempGitRepo();
        try {
            for (const eventName of ['push', 'merge_group', 'workflow_dispatch']) {
                const res = runSelect(dir, {EVENT_NAME: eventName, BASE_SHA: baseSha, HEAD_SHA: baseSha});
                assert.equal(res.status, 0, res.stderr);
                assert.deepEqual(parseOutputs(res.stdout), {
                    rust: 'true',
                    audit: 'true',
                    macos: 'true',
                });
            }
        } finally {
            fs.rmSync(dir, {recursive: true, force: true});
        }
    });

    it('narrows PR jobs accurately across docs, extension, pure Rust, portability, and Cargo changes', () => {
        const {dir, baseSha} = initTempGitRepo();
        try {
            // 1. Docs-only PR
            const docsHead = commitFiles(dir, {'README.md': '# updated\n'});
            let res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: baseSha, HEAD_SHA: docsHead});
            assert.equal(res.status, 0, res.stderr);
            assert.deepEqual(parseOutputs(res.stdout), {rust: 'false', audit: 'false', macos: 'false'});

            // 2. Extension-only PR
            const extHead = commitFiles(dir, {'extension/format.js': 'export const X = 1;\n'});
            res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: docsHead, HEAD_SHA: extHead});
            assert.equal(res.status, 0, res.stderr);
            assert.deepEqual(parseOutputs(res.stdout), {rust: 'false', audit: 'false', macos: 'false'});

            // 3. Pure Rust parser PR (Linux Rust suite runs; audit & macos skipped)
            const rustHead = commitFiles(dir, {'src/reset.rs': 'pub fn helper() {}\n'});
            res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: extHead, HEAD_SHA: rustHead});
            assert.equal(res.status, 0, res.stderr);
            assert.deepEqual(parseOutputs(res.stdout), {rust: 'true', audit: 'false', macos: 'false'});

            // 4. Rust PR introducing OS-conditional cfg in a non-listed module triggers macOS too
            const cfgHead = commitFiles(dir, {
                'src/reset.rs': '#[cfg(target_os = "macos")]\npub fn helper() {}\n',
            });
            res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: rustHead, HEAD_SHA: cfgHead});
            assert.equal(res.status, 0, res.stderr);
            assert.deepEqual(parseOutputs(res.stdout), {rust: 'true', audit: 'false', macos: 'true'});

            // 5. Cargo.lock PR triggers rust + audit + macos
            const lockHead = commitFiles(dir, {'Cargo.lock': '# lock\n'});
            res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: cfgHead, HEAD_SHA: lockHead});
            assert.equal(res.status, 0, res.stderr);
            assert.deepEqual(parseOutputs(res.stdout), {rust: 'true', audit: 'true', macos: 'true'});
        } finally {
            fs.rmSync(dir, {recursive: true, force: true});
        }
    });

    it('fails closed (red) on hidden/unclassified files or invalid/empty PR diffs', () => {
        const {dir, baseSha} = initTempGitRepo();
        try {
            // Hiding a change in an unclassified top-level file (e.g. build.rs) forces full matrix
            const hiddenHead = commitFiles(dir, {'build.rs': 'fn main() { panic!("broken"); }\n'});
            let res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: baseSha, HEAD_SHA: hiddenHead});
            assert.equal(res.status, 0);
            assert.deepEqual(parseOutputs(res.stdout), {rust: 'true', audit: 'true', macos: 'true'});

            // Missing BASE_SHA or empty diff exits non-zero so CI goes red rather than green
            res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: '', HEAD_SHA: hiddenHead});
            assert.notEqual(res.status, 0, 'Missing BASE_SHA must fail closed');

            res = runSelect(dir, {EVENT_NAME: 'pull_request', BASE_SHA: hiddenHead, HEAD_SHA: hiddenHead});
            assert.notEqual(res.status, 0, 'Empty PR diff must fail closed');
        } finally {
            fs.rmSync(dir, {recursive: true, force: true});
        }
    });
});
