import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {describe, it} from 'node:test';
import {fileURLToPath} from 'node:url';

import {
    CANONICAL_PROVIDERS,
    DEFAULT_PROVIDER_ORDER,
    PROVIDER_BY_ID,
    THEME_BY_ID,
    THEME_PRESETS,
    buildConfigUpsertArgs,
    buildTerminalArgv,
    clampPercent,
    computeBucketPace,
    computePanelSummary,
    defaultCredentialWatchPaths,
    evaluateQuotaNotificationStep,
    filterVisibleProviders,
    formatBucketDisplayLabel,
    formatBucketMetric,
    formatCompactAccountExtraNote,
    formatCreditsSummary,
    formatRelativeUpdated,
    formatResetCountdown,
    getActiveAccount,
    getBarFillRgba,
    getSeverityForUsedPercent,
    getThemePalette,
    groupBucketsForDetailedView,
    inferBucketWindowSeconds,
    normalizeStatusReport,
    posixShellQuote,
    slugifyAccountId,
    sortProvidersByOrder,
    tokenizeLoginCommand,
} from '../extension/format.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, '..');
const STYLESHEET_PATH = path.join(REPO_ROOT, 'extension', 'stylesheet.css');

// ---------------------------------------------------------------------------
// Helpers for WCAG 2.1 relative luminance & CSS rule contrast verification
// ---------------------------------------------------------------------------

function parseCssColor(raw) {
    const str = String(raw || '').trim();
    const hexMatch = /^#([0-9a-fA-F]{6})$/.exec(str);
    if (hexMatch) {
        const hex = hexMatch[1];
        return {
            r: parseInt(hex.slice(0, 2), 16),
            g: parseInt(hex.slice(2, 4), 16),
            b: parseInt(hex.slice(4, 6), 16),
            a: 1,
        };
    }
    const rgbaMatch = /^rgba?\(\s*([0-9.]+)\s*,\s*([0-9.]+)\s*,\s*([0-9.]+)(?:\s*,\s*([0-9.]+))?\s*\)$/.exec(str);
    if (rgbaMatch) {
        return {
            r: Number(rgbaMatch[1]),
            g: Number(rgbaMatch[2]),
            b: Number(rgbaMatch[3]),
            a: rgbaMatch[4] !== undefined ? Number(rgbaMatch[4]) : 1,
        };
    }
    throw new Error(`Unsupported CSS color literal: ${raw}`);
}

function compositeOver(fg, bg) {
    const alpha = fg.a + bg.a * (1 - fg.a);
    if (alpha <= 0)
        return {r: 0, g: 0, b: 0, a: 0};
    return {
        r: (fg.r * fg.a + bg.r * bg.a * (1 - fg.a)) / alpha,
        g: (fg.g * fg.a + bg.g * bg.a * (1 - fg.a)) / alpha,
        b: (fg.b * fg.a + bg.b * bg.a * (1 - fg.a)) / alpha,
        a: alpha,
    };
}

function channelToLinear(c8) {
    const s = c8 / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance(rgb) {
    return (
        0.2126 * channelToLinear(rgb.r) +
        0.7152 * channelToLinear(rgb.g) +
        0.0722 * channelToLinear(rgb.b)
    );
}

function contrastRatio(fgRaw, bgRaw, baseSurfaceRaw = null) {
    let bg = typeof bgRaw === 'string' ? parseCssColor(bgRaw) : bgRaw;
    if (baseSurfaceRaw && bg.a < 1) {
        const base = typeof baseSurfaceRaw === 'string' ? parseCssColor(baseSurfaceRaw) : baseSurfaceRaw;
        bg = compositeOver(bg, base);
    }
    let fg = typeof fgRaw === 'string' ? parseCssColor(fgRaw) : fgRaw;
    if (fg.a < 1)
        fg = compositeOver(fg, bg);

    const l1 = relativeLuminance(fg);
    const l2 = relativeLuminance(bg);
    const lighter = Math.max(l1, l2);
    const darker = Math.min(l1, l2);
    return (lighter + 0.05) / (darker + 0.05);
}

function extractThemeColorMap(cssText, themeSelectorPrefix) {
    const clean = cssText.replace(/\/\*[\s\S]*?\*\//g, '');
    const ruleRegex = /([^{}]+)\{([^{}]+)\}/g;
    const propsBySelector = new Map();
    let match;

    while ((match = ruleRegex.exec(clean)) !== null) {
        const rawSelectors = match[1].split(',').map(s => s.trim()).filter(Boolean);
        const body = match[2];
        const props = {};
        for (const decl of body.split(';')) {
            const colon = decl.indexOf(':');
            if (colon <= 0)
                continue;
            const k = decl.slice(0, colon).trim();
            const v = decl.slice(colon + 1).trim();
            if (k && v)
                props[k] = v;
        }

        for (const sel of rawSelectors) {
            if (sel === themeSelectorPrefix) {
                const prev = propsBySelector.get('__root__') ?? {};
                propsBySelector.set('__root__', {...prev, ...props});
            } else if (sel.startsWith(`${themeSelectorPrefix} `)) {
                const sub = sel.slice(themeSelectorPrefix.length + 1).trim();
                const prev = propsBySelector.get(sub) ?? {};
                propsBySelector.set(sub, {...prev, ...props});
            }
        }
    }

    return propsBySelector;
}

// ---------------------------------------------------------------------------
// 1. 7 Canonical Providers & Shared Contracts
// ---------------------------------------------------------------------------

describe('7 Canonical Providers & Shared Contracts', () => {
    it('exports all 7 canonical providers in exact default order', () => {
        assert.deepEqual(DEFAULT_PROVIDER_ORDER, [
            'claude',
            'codex',
            'grok',
            'agy',
            'cursor',
            'copilot',
            'gemini',
        ]);
        assert.equal(Object.isFrozen(DEFAULT_PROVIDER_ORDER), true);
        assert.deepEqual(
            CANONICAL_PROVIDERS.map(p => p.id),
            [...DEFAULT_PROVIDER_ORDER]
        );
    });

    it('exposes exact Copilot and Gemini metadata matching Rust & Preferences contracts', () => {
        const copilot = PROVIDER_BY_ID.copilot;
        assert.ok(copilot);
        assert.equal(copilot.displayName, 'GitHub Copilot');
        assert.equal(copilot.iconName, 'provider-copilot');
        assert.equal(copilot.defaultLoginCommand, 'gh auth login');
        assert.equal(copilot.dashboardUrl, 'https://github.com/settings/copilot');

        const gemini = PROVIDER_BY_ID.gemini;
        assert.ok(gemini);
        assert.equal(gemini.displayName, 'Gemini CLI');
        assert.equal(gemini.iconName, 'provider-gemini');
        assert.equal(gemini.defaultLoginCommand, 'gemini');
        assert.equal(gemini.dashboardUrl, 'https://aistudio.google.com/');
    });

    it('defines complete non-empty metadata for every canonical provider', () => {
        for (const id of DEFAULT_PROVIDER_ORDER) {
            const meta = PROVIDER_BY_ID[id];
            assert.ok(meta, `Missing PROVIDER_BY_ID.${id}`);
            assert.equal(typeof meta.displayName, 'string');
            assert.ok(meta.displayName.length > 0);
            assert.match(meta.iconName, /^provider-[a-z]+$/);
            assert.equal(typeof meta.defaultLoginCommand, 'string');
            assert.ok(meta.defaultLoginCommand.length > 0);
            assert.match(meta.dashboardUrl, /^https:\/\//);
        }
    });

    it('returns all 6 canonical credential watch paths and deduplicates custom XDG_CONFIG_HOME', () => {
        const standard = defaultCredentialWatchPaths('/home/alice', '/home/alice/.config');
        assert.deepEqual(standard, [
            '/home/alice/.claude/.credentials.json',
            '/home/alice/.codex/auth.json',
            '/home/alice/.grok/auth.json',
            '/home/alice/.config/gh/hosts.yml',
            '/home/alice/.config/github-copilot/hosts.json',
            '/home/alice/.gemini/oauth_creds.json',
        ]);

        const customXdg = defaultCredentialWatchPaths('/home/alice/', '/home/alice/.xdg-cfg/');
        assert.equal(customXdg.length, 8);
        assert.ok(customXdg.includes('/home/alice/.xdg-cfg/gh/hosts.yml'));
        assert.ok(customXdg.includes('/home/alice/.xdg-cfg/github-copilot/hosts.json'));
        assert.deepEqual(defaultCredentialWatchPaths(''), []);
    });

    it('exposes all 8 calm-tech theme palettes with valid RGBA bar tracks and fills', () => {
        assert.equal(THEME_PRESETS.length, 8);
        const expectedIds = [
            'midnight-slate',
            'system',
            'tokyo-night',
            'catppuccin',
            'gruvbox',
            'rose-pine',
            'nord',
            'everforest',
        ];
        assert.deepEqual(THEME_PRESETS.map(t => t.id), expectedIds);
        for (const id of expectedIds) {
            const p = getThemePalette(id);
            assert.equal(p.id, id);
            for (const rgba of [p.trackRgba, p.heroTrackRgba, p.okFillRgba, p.warnFillRgba, p.critFillRgba]) {
                assert.equal(rgba.length, 4);
                for (const c of rgba)
                    assert.ok(c >= 0 && c <= 1, `Channel out of [0,1] in ${id}: ${c}`);
            }
        }
        assert.equal(getThemePalette('non-existent').id, 'midnight-slate');
        assert.deepEqual(getBarFillRgba('critical', 'nord'), THEME_BY_ID.nord.critFillRgba);
        assert.deepEqual(getBarFillRgba('warning', 'gruvbox'), THEME_BY_ID.gruvbox.warnFillRgba);
        assert.deepEqual(getBarFillRgba('ok', 'catppuccin'), THEME_BY_ID.catppuccin.okFillRgba);
    });
});

// ---------------------------------------------------------------------------
// 2. Custom `provider-order` Sorting & Visibility
// ---------------------------------------------------------------------------

describe('sortProvidersByOrder & filterVisibleProviders', () => {
    const sampleProviders = DEFAULT_PROVIDER_ORDER.map(id => ({
        id,
        display_name: PROVIDER_BY_ID[id].displayName,
    }));

    it('preserves canonical order when given DEFAULT_PROVIDER_ORDER', () => {
        const sorted = sortProvidersByOrder(sampleProviders, DEFAULT_PROVIDER_ORDER);
        assert.deepEqual(sorted.map(p => p.id), [...DEFAULT_PROVIDER_ORDER]);
        assert.notEqual(sorted, sampleProviders);
    });

    it('reorders all 7 providers live according to custom user order', () => {
        const customOrder = ['gemini', 'copilot', 'cursor', 'claude', 'codex', 'grok', 'agy'];
        const sorted = sortProvidersByOrder(sampleProviders, customOrder);
        assert.deepEqual(sorted.map(p => p.id), customOrder);
    });

    it('appends omitted canonical providers in canonical order when orderIds is partial', () => {
        // Simulates upgrading from a 5-provider GSettings array so copilot & gemini are never lost
        const legacyFiveOrder = ['codex', 'claude', 'cursor', 'grok', 'agy'];
        const sorted = sortProvidersByOrder(sampleProviders, legacyFiveOrder);
        assert.deepEqual(sorted.map(p => p.id), [
            'codex',
            'claude',
            'cursor',
            'grok',
            'agy',
            'copilot',
            'gemini',
        ]);
    });

    it('deduplicates repeated IDs, ignores unknown order entries, and keeps extra report providers', () => {
        const withExperimental = [...sampleProviders, {id: 'custom-lab', display_name: 'Custom Lab'}];
        const messyOrder = ['gemini', 'gemini', 'unknown-id', 'claude', '', null];
        const sorted = sortProvidersByOrder(withExperimental, messyOrder);
        assert.deepEqual(sorted.map(p => p.id), [
            'gemini',
            'claude',
            'codex',
            'grok',
            'agy',
            'cursor',
            'copilot',
            'custom-lab',
        ]);
    });

    it('falls back to DEFAULT_PROVIDER_ORDER when orderIds is empty or invalid', () => {
        const reversed = [...sampleProviders].reverse();
        assert.deepEqual(
            sortProvidersByOrder(reversed, []).map(p => p.id),
            [...DEFAULT_PROVIDER_ORDER]
        );
        assert.deepEqual(
            sortProvidersByOrder(reversed, null).map(p => p.id),
            [...DEFAULT_PROVIDER_ORDER]
        );
        assert.deepEqual(sortProvidersByOrder(null, DEFAULT_PROVIDER_ORDER), []);
    });

    it('combines with filterVisibleProviders so hidden providers are excluded after ordering', () => {
        const ordered = sortProvidersByOrder(sampleProviders, ['copilot', 'gemini', 'claude']);
        const visible = filterVisibleProviders(ordered, ['gemini', 'grok', 'cursor']);
        assert.deepEqual(visible.map(p => p.id), ['copilot', 'claude', 'codex', 'agy']);
    });
});

// ---------------------------------------------------------------------------
// 3. Burn-Rate / Pace Indicator (`inferBucketWindowSeconds` & `computeBucketPace`)
// ---------------------------------------------------------------------------

describe('inferBucketWindowSeconds & computeBucketPace', () => {
    const NOW = 1_760_000_000;

    it('prefers explicit bucket.window_seconds from Rust UsageBucket', () => {
        assert.equal(inferBucketWindowSeconds({id: 'custom', window_seconds: 43_200}), 43_200);
        assert.equal(inferBucketWindowSeconds({id: 'wk', window_seconds: 120_000}), 120_000);
    });

    it('infers canonical window durations from bucket id, short_label, and label', () => {
        assert.equal(inferBucketWindowSeconds({id: 'session_5h', short_label: '5h'}), 18_000);
        assert.equal(inferBucketWindowSeconds({id: 'gemini_daily', short_label: '24h'}), 86_400);
        assert.equal(inferBucketWindowSeconds({label: 'Daily Flash'}), 86_400);
        assert.equal(inferBucketWindowSeconds({short_label: 'wk'}), 604_800);
        assert.equal(inferBucketWindowSeconds({label: '7d rolling'}), 604_800);
        assert.equal(inferBucketWindowSeconds({short_label: '31d'}), 2_678_400);
        assert.equal(inferBucketWindowSeconds({short_label: 'premium', label: 'Premium Requests'}), 2_678_400);
        assert.equal(inferBucketWindowSeconds({id: 'mystery', short_label: 'credits'}), 0);
    });

    it('returns null when resets_at_epoch is missing, expired, or used_percent is 0', () => {
        assert.equal(computeBucketPace({short_label: 'wk', used_percent: 45, resets_at_epoch: null}, NOW), null);
        assert.equal(computeBucketPace({short_label: 'wk', used_percent: 45, resets_at_epoch: NOW - 10}, NOW), null);
        assert.equal(computeBucketPace({short_label: 'wk', used_percent: 0, resets_at_epoch: NOW + 300_000}, NOW), null);
    });

    it('returns null when less than 5% or at least 99% of the window has elapsed', () => {
        // 3% elapsed of 18,000s (5h) -> remaining = 17,460s
        assert.equal(
            computeBucketPace({short_label: '5h', used_percent: 20, resets_at_epoch: NOW + 17_460}, NOW),
            null
        );
        // 99.2% elapsed of 18,000s -> remaining = 144s
        assert.equal(
            computeBucketPace({short_label: '5h', used_percent: 80, resets_at_epoch: NOW + 144}, NOW),
            null
        );
    });

    it('classifies comfortable under-budget pace (< 0.85x) cleanly', () => {
        // 25% elapsed of 604,800s (wk) -> remaining = 453,600s; 19% used -> ratio = 0.76, projected = 76%
        const pace = computeBucketPace({
            short_label: 'wk',
            used_percent: 19,
            resets_at_epoch: NOW + 453_600,
        }, NOW);
        assert.ok(pace);
        assert.equal(pace.state, 'under');
        assert.equal(pace.severity, 'normal');
        assert.equal(pace.projectedPercent, 76);
        assert.equal(pace.text, 'Pace: Comfortable · 19% used at 25% of window');
    });

    it('classifies on-track pace (0.85x .. 1.05x) with projected reset percentage', () => {
        // 50% elapsed of 18,000s (5h) -> remaining = 9,000s; 44% used -> ratio = 0.88, projected = 88%
        const pace = computeBucketPace({
            short_label: '5h',
            used_percent: 44,
            resets_at_epoch: NOW + 9_000,
        }, NOW);
        assert.ok(pace);
        assert.equal(pace.state, 'on_track');
        assert.equal(pace.severity, 'normal');
        assert.equal(pace.projectedPercent, 88);
        assert.equal(pace.text, 'Pace: On track · ~88% by reset');
    });

    it('classifies elevated pace (1.05x .. 1.35x) with warning severity when projected >= 95% & ratio > 1.15', () => {
        // 50% elapsed of 86,400s (24h) -> remaining = 43,200s; 60% used -> ratio = 1.20, projected = 120%
        const pace = computeBucketPace({
            short_label: '24h',
            used_percent: 60,
            resets_at_epoch: NOW + 43_200,
        }, NOW);
        assert.ok(pace);
        assert.equal(pace.state, 'elevated');
        assert.equal(pace.severity, 'warning');
        assert.equal(pace.projectedPercent, 120);
        assert.equal(pace.text, 'Pace: Elevated (1.2×) · hits 100% near reset');
    });

    it('classifies over-budget pace (> 1.35x) with critical severity', () => {
        // 25% elapsed of 2,678,400s (31d) -> remaining = 2,008,800s; 48% used -> ratio = 1.92, projected = 192%
        const pace = computeBucketPace({
            short_label: 'premium',
            used_percent: 48,
            resets_at_epoch: NOW + 2_008_800,
        }, NOW);
        assert.ok(pace);
        assert.equal(pace.state, 'over');
        assert.equal(pace.severity, 'critical');
        assert.equal(pace.projectedPercent, 192);
        assert.equal(pace.text, 'Pace: 1.9× sustainable · hits 100% before reset');
    });

    it('keeps severity normal when slightly elevated (ratio <= 1.15)', () => {
        // 50% elapsed of 18,000s -> remaining = 9,000s; 55% used -> ratio = 1.10, projected = 110%
        const pace = computeBucketPace({
            short_label: '5h',
            used_percent: 55,
            resets_at_epoch: NOW + 9_000,
        }, NOW);
        assert.ok(pace);
        assert.equal(pace.state, 'elevated');
        assert.equal(pace.severity, 'normal');
        assert.equal(pace.projectedPercent, 110);
    });
});

// ---------------------------------------------------------------------------
// 4. Report Normalization, Time-Decay, Bucket Labels & Panel Summary
// ---------------------------------------------------------------------------

describe('normalizeStatusReport, Labels & computePanelSummary', () => {
    const NOW = 1_760_000_000;

    it('formats bucket display labels across wk, 5h, 24h/daily, 31d, premium, and compound models', () => {
        assert.equal(formatBucketDisplayLabel({short_label: 'wk', label: 'wk'}), 'Weekly');
        assert.equal(formatBucketDisplayLabel({short_label: '5h', label: '5h'}), '5-Hour');
        assert.equal(formatBucketDisplayLabel({short_label: '24h', label: '24h'}), 'Daily');
        assert.equal(formatBucketDisplayLabel({short_label: 'daily', label: 'daily'}), 'Daily');
        assert.equal(formatBucketDisplayLabel({short_label: '31d', label: '31d'}), 'Monthly');
        assert.equal(formatBucketDisplayLabel({short_label: 'premium', label: 'premium'}), 'Premium Requests');
        assert.equal(formatBucketDisplayLabel({short_label: 'Sonnet · wk', label: ''}), 'Sonnet · Weekly');
        assert.equal(formatBucketDisplayLabel({short_label: 'Flash · 24h', label: ''}), 'Flash · Daily');
    });

    it('normalizes v2 reports across all 7 providers and decays expired buckets passively to 0%', () => {
        const raw = {
            schema_version: 2,
            updated_at_epoch: NOW - 30,
            providers: [
                {
                    id: 'copilot',
                    active_account_id: 'copilot:default',
                    accounts: [
                        {
                            id: 'copilot:default',
                            label: 'octocat',
                            is_active: true,
                            state: 'active_ok',
                            buckets: [
                                {
                                    id: 'premium',
                                    short_label: '31d',
                                    is_primary: true,
                                    used_percent: 64,
                                    window_seconds: 2_678_400,
                                    resets_at_epoch: NOW + 800_000,
                                },
                            ],
                        },
                    ],
                },
                {
                    id: 'gemini',
                    active_account_id: 'gemini:default',
                    accounts: [
                        {
                            id: 'gemini:default',
                            label: 'dev@example.com',
                            is_active: true,
                            state: 'active_ok',
                            buckets: [
                                {
                                    id: 'daily',
                                    short_label: '24h',
                                    is_primary: true,
                                    used_percent: 91,
                                    window_seconds: 86_400,
                                    // Expired 10 seconds ago -> passive time-decay must reset to 0% used / 100% rem
                                    resets_at_epoch: NOW - 10,
                                },
                            ],
                        },
                    ],
                },
            ],
        };

        const report = normalizeStatusReport(raw, NOW);
        assert.equal(report.providers.length, 2);

        const copilot = report.providers[0];
        assert.equal(copilot.display_name, 'GitHub Copilot');
        assert.equal(copilot.icon_name, 'provider-copilot');
        assert.equal(copilot.dashboard_url, 'https://github.com/settings/copilot');
        assert.equal(copilot.primary_used_percent, 64);
        assert.equal(copilot.accounts[0].login_command, 'gh auth login');
        assert.equal(copilot.accounts[0].buckets[0].window_seconds, 2_678_400);

        const gemini = report.providers[1];
        assert.equal(gemini.display_name, 'Gemini CLI');
        assert.equal(gemini.icon_name, 'provider-gemini');
        assert.equal(gemini.dashboard_url, 'https://aistudio.google.com/');
        assert.equal(gemini.primary_used_percent, 0);
        assert.equal(gemini.primary_remaining_percent, 100);
        assert.equal(gemini.accounts[0].buckets[0].used_percent, 0);
        assert.equal(gemini.accounts[0].buckets[0].remaining_percent, 100);
    });

    it('upgrades legacy v1 agents reports cleanly to v2 provider schema', () => {
        const legacy = {
            updated_at_epoch: NOW,
            agents: [
                {agent_type: 'copilot', name: 'GitHub Copilot', used_percent: 42},
                {agent_type: 'gemini', name: 'Gemini CLI', error: 'Not signed in'},
            ],
        };
        const report = normalizeStatusReport(legacy, NOW);
        assert.equal(report.schema_version, 2);
        assert.equal(report.providers.length, 2);
        assert.equal(report.providers[0].id, 'copilot');
        assert.equal(report.providers[0].primary_used_percent, 42);
        assert.equal(report.providers[0].accounts[0].login_command, 'gh auth login');
        assert.equal(report.providers[1].id, 'gemini');
        assert.equal(report.providers[1].accounts[0].state, 'error');
    });

    it('computes panel summary pills, fullText, metricMode, and worstSeverity in custom order', () => {
        const report = normalizeStatusReport({
            schema_version: 2,
            providers: [
                {
                    id: 'claude',
                    accounts: [{
                        id: 'c1',
                        is_active: true,
                        state: 'active_ok',
                        buckets: [{id: 'wk', short_label: 'wk', is_primary: true, used_percent: 35}],
                    }],
                },
                {
                    id: 'copilot',
                    accounts: [{
                        id: 'cp1',
                        is_active: true,
                        state: 'active_ok',
                        buckets: [{id: '31d', short_label: '31d', is_primary: true, used_percent: 84}],
                    }],
                },
                {
                    id: 'gemini',
                    accounts: [{
                        id: 'g1',
                        is_active: true,
                        state: 'signed_out',
                        buckets: [],
                    }],
                },
            ],
        }, NOW);

        const ordered = sortProvidersByOrder(report.providers, ['copilot', 'claude', 'gemini']);
        const usedSummary = computePanelSummary(ordered, 'used', ' · ');
        assert.equal(usedSummary.severity, 'warning');
        assert.equal(usedSummary.hasAuthError, true);
        assert.equal(usedSummary.pills.length, 3);
        assert.equal(usedSummary.pills[0].id, 'copilot');
        assert.equal(usedSummary.pills[0].percentText, '84%');
        assert.equal(usedSummary.pills[1].id, 'claude');
        assert.equal(usedSummary.pills[1].percentText, '35%');
        assert.equal(usedSummary.pills[2].id, 'gemini');
        assert.equal(usedSummary.pills[2].percentText, '!');
        assert.equal(usedSummary.fullText, 'GitHub Copilot: 84% · Claude: 35% · Gemini CLI: AUTH');

        const remSummary = computePanelSummary(ordered, 'remaining');
        assert.equal(remSummary.pills[0].percentText, '16%');
        assert.equal(remSummary.pills[1].percentText, '65%');
        assert.equal(remSummary.fullText, 'GitHub Copilot: 16% | Claude: 65% | Gemini CLI: AUTH');
    });

    it('handles helper formatters (severity, metrics, countdowns, credits, slugs, CLI upsert args)', () => {
        assert.equal(clampPercent(-12), 0);
        assert.equal(clampPercent(142), 100);
        assert.equal(clampPercent(Number.NaN), 0);

        assert.equal(getSeverityForUsedPercent(79), 'normal');
        assert.equal(getSeverityForUsedPercent(80), 'warning');
        assert.equal(getSeverityForUsedPercent(89), 'warning');
        assert.equal(getSeverityForUsedPercent(90), 'critical');

        assert.equal(formatBucketMetric({used_percent: 25, remaining_percent: 75}, 'used').percentText, '25%');
        assert.equal(formatBucketMetric({used_percent: 25, remaining_percent: 75}, 'used').heroCaption, '25% used');
        assert.equal(formatBucketMetric({used_percent: 25, remaining_percent: 75}, 'remaining').percentText, '75%');
        assert.equal(formatBucketMetric({used_percent: 25, remaining_percent: 75}, 'remaining').heroCaption, '75% left');

        assert.equal(formatResetCountdown(NOW + 9_000, NOW), 'Resets in 2h 30m');
        assert.equal(formatResetCountdown(NOW + 180_000, NOW, {prefix: false}), '2d 2h');
        assert.equal(formatRelativeUpdated(NOW - 125, NOW), 'Updated 2m ago');
        assert.equal(formatCreditsSummary({balance_dollars: 14.5}), 'Credits: $14.50 available');
        assert.equal(
            formatCompactAccountExtraNote({credits: {balance_dollars: 12}}, false),
            'Credits: $12.00'
        );
        assert.equal(
            getActiveAccount({
                active_account_id: 'b',
                accounts: [{id: 'a', is_active: false}, {id: 'b', is_active: true}],
            })?.id,
            'b'
        );
        assert.equal(groupBucketsForDetailedView([{id: 'a'}, {id: 'b'}]).length, 2);
        assert.equal(slugifyAccountId('  Work Account #2! '), 'work-account-2');

        const args = buildConfigUpsertArgs({
            provider: 'copilot',
            id: 'Work Profile',
            label: 'Work Profile',
            path: '~/.config/gh/hosts.yml',
            pollMode: 'active',
        });
        assert.deepEqual(args, [
            'config',
            'upsert-account',
            '--provider',
            'copilot',
            '--id',
            'work-profile',
            '--label',
            'Work Profile',
            '--path',
            '~/.config/gh/hosts.yml',
            '--poll-mode',
            'active',
        ]);
    });
});

// ---------------------------------------------------------------------------
// 5. Smart Quota & Reset Desktop Notifications (`evaluateQuotaNotificationStep`)
// ---------------------------------------------------------------------------

describe('evaluateQuotaNotificationStep', () => {
    const NOW = 1_760_000_000;
    const WIN_1 = NOW + 172_800; // +2d
    const WIN_2 = WIN_1 + 604_800; // next week

    function makeSingleBucketReport(providerId, usedPercent, resetsAtEpoch = WIN_1) {
        return {
            schema_version: 2,
            updated_at_epoch: NOW,
            providers: [
                {
                    id: providerId,
                    active_account_id: `${providerId}:default`,
                    accounts: [
                        {
                            id: `${providerId}:default`,
                            label: 'Default',
                            is_active: true,
                            state: 'active_ok',
                            buckets: [
                                {
                                    id: 'wk',
                                    short_label: 'wk',
                                    is_primary: true,
                                    used_percent: usedPercent,
                                    resets_at_epoch: resetsAtEpoch,
                                },
                            ],
                        },
                    ],
                },
            ],
        };
    }

    it('seeds baseline on initial hydration without firing notifications even at >= 90%', () => {
        const states = new Map();
        const out = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('claude', 94, WIN_1),
            {
                isInitialHydration: true,
                nowEpoch: NOW,
                notifyOnWarning: true,
                notifyOnCritical: true,
                notifyOnReset: true,
            }
        );
        assert.deepEqual(out, []);
        assert.equal(states.size, 1);

        // Subsequent update at 96% in the same window also does not spam
        const out2 = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('claude', 96, WIN_1),
            {nowEpoch: NOW + 60, notifyOnWarning: true, notifyOnCritical: true}
        );
        assert.deepEqual(out2, []);
    });

    it('fires warning once per reset window on <80% -> >=80% (<90%) only when notifyOnWarning is true', () => {
        const states = new Map();
        evaluateQuotaNotificationStep(states, makeSingleBucketReport('claude', 75, WIN_1), {
            isInitialHydration: true,
            nowEpoch: NOW,
        });

        // When notifyOnWarning is false (default), crossing 75 -> 82 emits nothing
        const muted = evaluateQuotaNotificationStep(
            new Map(states),
            makeSingleBucketReport('claude', 82, WIN_1),
            {nowEpoch: NOW + 60, notifyOnWarning: false}
        );
        assert.deepEqual(muted, []);

        // When notifyOnWarning is true, crossing 75 -> 82 emits once
        const fired = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('claude', 82, WIN_1),
            {nowEpoch: NOW + 60, notifyOnWarning: true}
        );
        assert.equal(fired.length, 1);
        assert.equal(fired[0].kind, 'warning');
        assert.equal(fired[0].urgency, 'normal');
        assert.equal(fired[0].title, 'Claude Quota Warning');
        assert.match(fired[0].body, /Weekly usage reached 82%/);

        // Dipping to 79% and back to 84% in the SAME reset window must NOT re-fire warning
        evaluateQuotaNotificationStep(states, makeSingleBucketReport('claude', 79, WIN_1), {
            nowEpoch: NOW + 120,
            notifyOnWarning: true,
        });
        const duplicate = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('claude', 84, WIN_1),
            {nowEpoch: NOW + 180, notifyOnWarning: true}
        );
        assert.deepEqual(duplicate, []);
    });

    it('fires critical notification once per reset window on <90% -> >=90%', () => {
        const states = new Map();
        evaluateQuotaNotificationStep(states, makeSingleBucketReport('codex', 85, WIN_1), {
            isInitialHydration: true,
            nowEpoch: NOW,
        });

        const fired = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('codex', 92, WIN_1),
            {nowEpoch: NOW + 60, notifyOnCritical: true}
        );
        assert.equal(fired.length, 1);
        assert.equal(fired[0].kind, 'critical');
        assert.equal(fired[0].urgency, 'critical');
        assert.equal(fired[0].title, 'Codex Quota Critical (92%)');
        assert.match(fired[0].body, /Weekly usage reached 92%/);

        // Further climb 92 -> 98 in same window does not re-fire
        const again = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('codex', 98, WIN_1),
            {nowEpoch: NOW + 120, notifyOnCritical: true}
        );
        assert.deepEqual(again, []);
    });

    it('fires quota reset notification when a high-usage (>=80%) window resets and re-arms next window', () => {
        const states = new Map();
        evaluateQuotaNotificationStep(states, makeSingleBucketReport('copilot', 91, WIN_1), {
            isInitialHydration: true,
            nowEpoch: NOW,
        });

        // New reset window arrives with 0% used
        const resetOut = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('copilot', 0, WIN_2),
            {nowEpoch: WIN_1 + 5, notifyOnReset: true}
        );
        assert.equal(resetOut.length, 1);
        assert.equal(resetOut[0].kind, 'reset');
        assert.equal(resetOut[0].title, 'GitHub Copilot Quota Reset');
        assert.equal(resetOut[0].body, 'Weekly quota is fresh again (0% used)');

        // Crossing 90% in the NEW window (WIN_2) fires critical cleanly
        const critNewWindow = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('copilot', 93, WIN_2),
            {nowEpoch: WIN_1 + 3600, notifyOnCritical: true}
        );
        assert.equal(critNewWindow.length, 1);
        assert.equal(critNewWindow[0].kind, 'critical');
    });

    it('suppresses notifications for hidden providers', () => {
        const states = new Map();
        evaluateQuotaNotificationStep(states, makeSingleBucketReport('gemini', 50, WIN_1), {
            isInitialHydration: true,
            nowEpoch: NOW,
        });
        const out = evaluateQuotaNotificationStep(
            states,
            makeSingleBucketReport('gemini', 95, WIN_1),
            {
                nowEpoch: NOW + 60,
                hiddenProviderIds: ['gemini'],
                notifyOnCritical: true,
            }
        );
        assert.deepEqual(out, []);
    });
});

// ---------------------------------------------------------------------------
// 6. Shell Command Hardening (`posixShellQuote`, `tokenizeLoginCommand`, `buildTerminalArgv`)
// ---------------------------------------------------------------------------

describe('Shell Command Hardening & Injection Prevention', () => {
    it('quotes arbitrary strings safely in posixShellQuote and strips NUL bytes', () => {
        assert.equal(posixShellQuote('claude'), "'claude'");
        assert.equal(posixShellQuote("a'b\0c"), "'a'\\''bc'");
        assert.equal(posixShellQuote('$(rm -rf /) `id`; | &'), "'$(rm -rf /) `id`; | &'");
    });

    it('tokenizes quoted paths with spaces while keeping shell metacharacters literal', () => {
        assert.deepEqual(
            tokenizeLoginCommand('"/opt/My AI Tools/claude" auth login'),
            ['/opt/My AI Tools/claude', 'auth', 'login']
        );
        assert.deepEqual(
            tokenizeLoginCommand('CLAUDE_CONFIG_DIR="/home/u/My Profile" claude auth login'),
            ['CLAUDE_CONFIG_DIR=/home/u/My Profile', 'claude', 'auth', 'login']
        );
    });

    it('builds correct terminal argv across ptyxis, gnome-terminal, alacritty, kitty, and agy fallback', () => {
        const ptyxis = buildTerminalArgv('/usr/bin/ptyxis', 'gh auth login');
        assert.deepEqual(ptyxis.slice(0, 5), ['/usr/bin/ptyxis', '--new-window', '--', 'sh', '-c']);
        assert.match(ptyxis[5], /'gh' 'auth' 'login'/);

        const gterm = buildTerminalArgv('/usr/bin/gnome-terminal', 'gemini');
        assert.deepEqual(gterm.slice(0, 4), ['/usr/bin/gnome-terminal', '--', 'sh', '-c']);
        assert.match(gterm[4], /'gemini'/);

        const alacritty = buildTerminalArgv('/usr/bin/alacritty', 'codex login');
        assert.deepEqual(alacritty.slice(0, 4), ['/usr/bin/alacritty', '-e', 'sh', '-c']);

        const kitty = buildTerminalArgv('/usr/bin/kitty', 'agy auth login');
        assert.deepEqual(kitty.slice(0, 3), ['/usr/bin/kitty', 'sh', '-c']);
        assert.match(kitty[3], /command -v agy/);

        const withEnv = buildTerminalArgv(
            '/usr/bin/ptyxis',
            'CLAUDE_CONFIG_DIR=~/.claude-work claude auth login',
            {homeDir: '/home/tester'}
        );
        assert.match(
            withEnv[5],
            /env 'CLAUDE_CONFIG_DIR=\/home\/tester\/\.claude-work' 'claude' 'auth' 'login'/
        );
    });

    it('prevents shell command injection in real /bin/sh across metacharacters and spaces', () => {
        const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'gas-shell-sec-'));
        try {
            const markerPath = path.join(tmpDir, 'injected-marker');
            const spacedDir = path.join(tmpDir, 'My Custom Bin');
            fs.mkdirSync(spacedDir, {recursive: true});
            const harmlessBin = path.join(spacedDir, 'mock-login');
            const argsCapturePath = path.join(tmpDir, 'captured-args.json');
            fs.writeFileSync(
                harmlessBin,
                `#!/bin/sh\nprintf '%s\\n' "$@" > "${argsCapturePath}"\n`,
                {mode: 0o755}
            );

            const payloads = [
                `"${harmlessBin}"; touch "${markerPath}"`,
                `"${harmlessBin}" $(touch "${markerPath}")`,
                `"${harmlessBin}" \`touch "${markerPath}"\``,
                `"${harmlessBin}" | touch "${markerPath}"`,
                `"${harmlessBin}" && touch "${markerPath}"`,
                `"${harmlessBin}"\ntouch "${markerPath}"`,
                `FOO="val'; touch ${markerPath}; '" "${harmlessBin}" --ok`,
            ];

            for (const payload of payloads) {
                const argv = buildTerminalArgv('/usr/bin/kitty', payload, {homeDir: tmpDir});
                // Strip the interactive `read -r _` tail so /bin/sh exits immediately in test
                const nonInteractive = argv[3].replace(/;\s*printf[\s\S]*$/, '');
                try {
                    execFileSync('/bin/sh', ['-c', nonInteractive], {
                        stdio: 'ignore',
                        timeout: 3000,
                    });
                } catch (_e) {
                    // Expected when payload corrupts argv[0] into a non-existent filename
                }
                assert.equal(
                    fs.existsSync(markerPath),
                    false,
                    `Shell injection marker was created for payload: ${payload}`
                );
            }

            // Also verify pretokenizedArgv executes a binary path containing spaces cleanly
            const pretokArgv = buildTerminalArgv('/usr/bin/kitty', harmlessBin, {
                homeDir: tmpDir,
                pretokenizedArgv: [harmlessBin, 'arg with spaces'],
            });
            const script = pretokArgv[3].replace(/;\s*printf[\s\S]*$/, '');
            execFileSync('/bin/sh', ['-c', script], {stdio: 'ignore', timeout: 3000});
            const captured = fs.readFileSync(argsCapturePath, 'utf8').trim();
            assert.equal(captured, 'arg with spaces');
        } finally {
            fs.rmSync(tmpDir, {recursive: true, force: true});
        }
    });
});

// ---------------------------------------------------------------------------
// 7. WCAG AA Contrast Ratios Across All 8 Themes (+ Light Host Surface)
// ---------------------------------------------------------------------------

describe('WCAG AA Contrast Ratios across all 8 stylesheet themes', () => {
    const cssText = fs.readFileSync(STYLESHEET_PATH, 'utf8');

    const themeSpecs = [
        {name: 'midnight-slate', selector: '.gas-popover.theme-midnight-slate', hostBg: '#1e1f2f'},
        {name: 'system (dark)', selector: '.gas-popover.theme-system', hostBg: '#242428'},
        {name: 'system (light)', selector: '.gas-popover.theme-system.is-light-surface', hostBg: '#fafafa'},
        {name: 'tokyo-night', selector: '.gas-popover.theme-tokyo-night', hostBg: '#1a1b26'},
        {name: 'catppuccin', selector: '.gas-popover.theme-catppuccin', hostBg: '#1e1e2e'},
        {name: 'gruvbox', selector: '.gas-popover.theme-gruvbox', hostBg: '#282828'},
        {name: 'rose-pine', selector: '.gas-popover.theme-rose-pine', hostBg: '#1f1d2e'},
        {name: 'nord', selector: '.gas-popover.theme-nord', hostBg: '#2e3440'},
        {name: 'everforest', selector: '.gas-popover.theme-everforest', hostBg: '#2d353b'},
    ];

    for (const spec of themeSpecs) {
        it(`meets >= 4.5:1 WCAG AA contrast across key text & pace pills in ${spec.name}`, () => {
            const map = extractThemeColorMap(cssText, spec.selector);
            const root = map.get('__root__') ?? {};
            const popoverBg =
                root['background-color'] && root['background-color'] !== 'transparent'
                    ? root['background-color']
                    : spec.hostBg;
            const heroBgRaw = map.get('.gas-hero-card')?.['background-color'] ?? popoverBg;
            const heroBg = compositeOver(parseCssColor(heroBgRaw), parseCssColor(popoverBg));

            const checks = [
                {label: 'primary text', fg: root.color, bg: popoverBg},
                {label: 'header subtitle', fg: map.get('.gas-header-subtitle')?.color, bg: popoverBg},
                {label: 'status note', fg: map.get('.gas-status-note')?.color, bg: popoverBg},
                {label: 'signed-out hint', fg: map.get('.gas-signed-out-hint')?.color, bg: popoverBg},
                {label: 'bucket warning pct', fg: map.get('.gas-bucket-pct.is-warning')?.color, bg: popoverBg},
                {label: 'bucket critical pct', fg: map.get('.gas-bucket-pct.is-critical')?.color, bg: popoverBg},
                {
                    label: 'hero pace pill (normal)',
                    fg: map.get('.gas-hero-pace-pill')?.color,
                    bg: map.get('.gas-hero-pace-pill')?.['background-color'] ?? heroBg,
                    base: heroBg,
                },
                {
                    label: 'hero pace pill (warning)',
                    fg: map.get('.gas-hero-pace-pill.is-warning')?.color,
                    bg: map.get('.gas-hero-pace-pill.is-warning')?.['background-color'] ?? heroBg,
                    base: heroBg,
                },
                {
                    label: 'hero pace pill (critical)',
                    fg: map.get('.gas-hero-pace-pill.is-critical')?.color,
                    bg: map.get('.gas-hero-pace-pill.is-critical')?.['background-color'] ?? heroBg,
                    base: heroBg,
                },
                {
                    label: 'error banner',
                    fg: map.get('.gas-error-banner')?.color,
                    bg: map.get('.gas-error-banner')?.['background-color'] ?? popoverBg,
                    base: popoverBg,
                },
            ];

            for (const c of checks) {
                assert.ok(c.fg, `Missing foreground color for ${c.label} in ${spec.name}`);
                const ratio = contrastRatio(c.fg, c.bg, c.base ?? null);
                assert.ok(
                    ratio >= 4.5,
                    `${spec.name} -> ${c.label} (${c.fg}) contrast ${ratio.toFixed(2)}:1 < 4.5:1`
                );
            }
        });
    }
});
