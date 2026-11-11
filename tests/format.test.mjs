import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {describe, it} from 'node:test';
import {fileURLToPath} from 'node:url';

import {
    CANONICAL_PROVIDERS,
    DEFAULT_HIDDEN_PROVIDERS,
    DEFAULT_PROVIDER_ORDER,
    POPOVER_LAYOUTS,
    PROVIDER_BY_ID,
    QUICK_THEME_IDS,
    SYSTEM_LIGHT_PALETTE,
    THEME_BY_ID,
    THEME_CATEGORIES,
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
    formatClipboardSummary,
    formatCompactAccountExtraNote,
    formatCreditsSummary,
    formatRelativeUpdated,
    formatResetCountdown,
    getActiveAccount,
    getBarFillRgba,
    getProviderAlertSeverity,
    getSeverityForUsedPercent,
    getThemePalette,
    groupBucketsForDetailedView,
    inferBucketWindowSeconds,
    normalizeSparklinePoints,
    normalizeStatusReport,
    posixShellQuote,
    resolveEffectivePopoverLayout,
    slugifyAccountId,
    sortProvidersByOrder,
    summarizeOverflowProviders,
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
// 1. 10 Canonical Providers, Default Top-5 Priority & 24 Curated Themes
// ---------------------------------------------------------------------------

describe('10 Canonical Providers, Top-5 Priority & 24 Theme Contracts', () => {
    it('exports all 10 canonical providers in exact default priority order and top-5 visibility', () => {
        assert.deepEqual(DEFAULT_PROVIDER_ORDER, [
            'claude',
            'codex',
            'grok',
            'cursor',
            'agy',
            'openrouter',
            'copilot',
            'gemini',
            'windsurf',
            'deepseek',
        ]);
        assert.deepEqual(DEFAULT_HIDDEN_PROVIDERS, [
            'openrouter',
            'copilot',
            'gemini',
            'windsurf',
            'deepseek',
        ]);
        assert.equal(Object.isFrozen(DEFAULT_PROVIDER_ORDER), true);
        assert.equal(Object.isFrozen(DEFAULT_HIDDEN_PROVIDERS), true);
        assert.deepEqual(
            CANONICAL_PROVIDERS.map(p => p.id),
            [...DEFAULT_PROVIDER_ORDER]
        );
    });

    it('exposes exact OpenRouter, Windsurf, DeepSeek, Copilot, and Gemini metadata', () => {
        assert.deepEqual(PROVIDER_BY_ID.openrouter, {
            id: 'openrouter',
            displayName: 'OpenRouter',
            iconName: 'provider-openrouter',
            shortCode: 'OR',
            defaultLoginCommand: 'openrouter',
            locationHint: '~/.config/openrouter/api_key',
            dashboardUrl: 'https://openrouter.ai/activity',
        });

        assert.deepEqual(PROVIDER_BY_ID.windsurf, {
            id: 'windsurf',
            displayName: 'Windsurf',
            iconName: 'provider-windsurf',
            shortCode: 'WS',
            defaultLoginCommand: 'windsurf',
            locationHint: '~/.config/Windsurf/User/globalStorage/state.vscdb',
            dashboardUrl: 'https://windsurf.com/subscription',
        });

        assert.deepEqual(PROVIDER_BY_ID.deepseek, {
            id: 'deepseek',
            displayName: 'DeepSeek',
            iconName: 'provider-deepseek',
            shortCode: 'DS',
            defaultLoginCommand: 'deepseek',
            locationHint: '~/.config/deepseek/api_key',
            dashboardUrl: 'https://platform.deepseek.com/usage',
        });

        assert.equal(PROVIDER_BY_ID.copilot.displayName, 'GitHub Copilot');
        assert.equal(PROVIDER_BY_ID.copilot.iconName, 'provider-copilot');
        assert.equal(PROVIDER_BY_ID.copilot.defaultLoginCommand, 'gh auth login');
        assert.equal(PROVIDER_BY_ID.copilot.dashboardUrl, 'https://github.com/settings/copilot');

        assert.equal(PROVIDER_BY_ID.gemini.displayName, 'Gemini CLI');
        assert.equal(PROVIDER_BY_ID.gemini.iconName, 'provider-gemini');
        assert.equal(PROVIDER_BY_ID.gemini.defaultLoginCommand, 'gemini');
        assert.equal(PROVIDER_BY_ID.gemini.dashboardUrl, 'https://aistudio.google.com/');
    });

    it('defines complete non-empty metadata for every canonical provider', () => {
        for (const id of DEFAULT_PROVIDER_ORDER) {
            const meta = PROVIDER_BY_ID[id];
            assert.ok(meta, `Missing PROVIDER_BY_ID.${id}`);
            assert.equal(typeof meta.displayName, 'string');
            assert.ok(meta.displayName.length > 0);
            assert.match(meta.iconName, /^provider-[a-z]+$/);
            assert.equal(typeof meta.shortCode, 'string');
            assert.equal(meta.shortCode.length, 2);
            assert.equal(typeof meta.defaultLoginCommand, 'string');
            assert.ok(meta.defaultLoginCommand.length > 0);
            assert.match(meta.dashboardUrl, /^https:\/\//);
        }
    });

    it('returns all 9 canonical credential watch paths and deduplicates custom XDG_CONFIG_HOME', () => {
        const standard = defaultCredentialWatchPaths('/home/alice', '/home/alice/.config');
        assert.deepEqual(standard, [
            '/home/alice/.claude/.credentials.json',
            '/home/alice/.codex/auth.json',
            '/home/alice/.grok/auth.json',
            '/home/alice/.config/gh/hosts.yml',
            '/home/alice/.config/github-copilot/hosts.json',
            '/home/alice/.gemini/oauth_creds.json',
            '/home/alice/.config/openrouter',
            '/home/alice/.config/Windsurf/User/globalStorage/state.vscdb',
            '/home/alice/.config/deepseek',
        ]);

        const customXdg = defaultCredentialWatchPaths('/home/alice/', '/home/alice/.xdg-cfg/');
        assert.equal(customXdg.length, 14);
        assert.ok(customXdg.includes('/home/alice/.xdg-cfg/openrouter'));
        assert.ok(customXdg.includes('/home/alice/.xdg-cfg/Windsurf/User/globalStorage/state.vscdb'));
        assert.ok(customXdg.includes('/home/alice/.xdg-cfg/deepseek'));
        assert.deepEqual(defaultCredentialWatchPaths(''), []);
    });

    it('exposes all 24 curated theme presets + system-light across glass, light, and dark categories', () => {
        assert.equal(THEME_PRESETS.length, 24);
        const expectedIds = [
            'system',
            'liquid-glass',
            'liquid-glass-light',
            'catppuccin-latte',
            'flexoki-light',
            'rose-pine-dawn',
            'daylight-white',
            'lumon',
            'midnight-slate',
            'tokyo-night',
            'catppuccin',
            'everforest',
            'gruvbox',
            'kanagawa',
            'matte-black',
            'nord',
            'osaka-jade',
            'ristretto',
            'rose-pine',
            'ethereal',
            'hackerman',
            'miasma',
            'vantablack',
            'retro-82',
        ];
        assert.deepEqual(THEME_PRESETS.map(t => t.id), expectedIds);
        assert.equal(THEME_PRESETS.filter(t => t.category === 'glass').length, 3);
        assert.equal(THEME_PRESETS.filter(t => t.category === 'light').length, 5);
        assert.equal(THEME_PRESETS.filter(t => t.category === 'dark').length, 16);

        assert.deepEqual(QUICK_THEME_IDS, [
            'midnight-slate',
            'liquid-glass',
            'catppuccin-latte',
            'tokyo-night',
        ]);
        assert.deepEqual(THEME_CATEGORIES.map(c => c.id), ['all', 'dark', 'light', 'glass']);
        assert.deepEqual(POPOVER_LAYOUTS.map(l => l.id), ['list', 'auto-grid', 'grid']);

        for (const id of [...expectedIds, 'system-light']) {
            const p = getThemePalette(id);
            assert.equal(p.id, id);
            assert.match(p.swatchHex, /^#[0-9a-fA-F]{6}$/);
            assert.match(p.accentHex, /^#[0-9a-fA-F]{6}$/);
            assert.match(p.bgHex, /^#[0-9a-fA-F]{6}$/);
            assert.match(p.fgHex, /^#[0-9a-fA-F]{6}$/);
            assert.match(p.mutedHex, /^#[0-9a-fA-F]{6}$/);
            for (const rgba of [p.trackRgba, p.heroTrackRgba, p.okFillRgba, p.warnFillRgba, p.critFillRgba]) {
                assert.equal(rgba.length, 4);
                for (const c of rgba)
                    assert.ok(c >= 0 && c <= 1, `Channel out of [0,1] in ${id}: ${c}`);
            }
        }
        assert.equal(SYSTEM_LIGHT_PALETTE.isLight, true);
        assert.equal(getThemePalette('non-existent').id, 'midnight-slate');
        assert.deepEqual(getBarFillRgba('critical', 'nord'), THEME_BY_ID.nord.critFillRgba);
        assert.deepEqual(getBarFillRgba('warning', 'gruvbox'), THEME_BY_ID.gruvbox.warnFillRgba);
        assert.deepEqual(getBarFillRgba('normal', 'catppuccin'), THEME_BY_ID.catppuccin.okFillRgba);
    });
});

// ---------------------------------------------------------------------------
// 2. Custom `provider-order` Sorting, Visibility, Layout & Progressive Disclosure
// ---------------------------------------------------------------------------

describe('sortProvidersByOrder, filterVisibleProviders, Layout & Overflow Disclosure', () => {
    const sampleProviders = DEFAULT_PROVIDER_ORDER.map(id => ({
        id,
        display_name: PROVIDER_BY_ID[id].displayName,
    }));

    it('preserves canonical 10-provider order and filters DEFAULT_HIDDEN_PROVIDERS to top 5', () => {
        const sorted = sortProvidersByOrder(sampleProviders, DEFAULT_PROVIDER_ORDER);
        assert.deepEqual(sorted.map(p => p.id), [...DEFAULT_PROVIDER_ORDER]);
        assert.notEqual(sorted, sampleProviders);

        const defaultVisible = filterVisibleProviders(sorted, DEFAULT_HIDDEN_PROVIDERS);
        assert.deepEqual(defaultVisible.map(p => p.id), [
            'claude',
            'codex',
            'grok',
            'cursor',
            'agy',
        ]);
    });

    it('appends omitted canonical providers in canonical order when upgrading from 5 or 7 providers', () => {
        const legacySevenOrder = ['codex', 'claude', 'cursor', 'grok', 'agy', 'copilot', 'gemini'];
        const sorted = sortProvidersByOrder(sampleProviders, legacySevenOrder);
        assert.deepEqual(sorted.map(p => p.id), [
            'codex',
            'claude',
            'cursor',
            'grok',
            'agy',
            'copilot',
            'gemini',
            'openrouter',
            'windsurf',
            'deepseek',
        ]);
    });

    it('deduplicates repeated IDs, ignores unknown order entries, and keeps extra report providers', () => {
        const withExperimental = [...sampleProviders, {id: 'custom-lab', display_name: 'Custom Lab'}];
        const messyOrder = ['deepseek', 'deepseek', 'unknown-id', 'claude', '', null];
        const sorted = sortProvidersByOrder(withExperimental, messyOrder);
        assert.deepEqual(sorted.map(p => p.id), [
            'deepseek',
            'claude',
            'codex',
            'grok',
            'cursor',
            'agy',
            'openrouter',
            'copilot',
            'gemini',
            'windsurf',
            'custom-lab',
        ]);
    });

    it('resolves list vs 2-column wide Bento layout across list, auto-grid, and grid modes', () => {
        assert.equal(resolveEffectivePopoverLayout('list', 10, 2560), 'list');
        assert.equal(resolveEffectivePopoverLayout('grid', 1, 1920), 'list');
        assert.equal(resolveEffectivePopoverLayout('grid', 2, 1920), 'grid');
        assert.equal(resolveEffectivePopoverLayout('auto-grid', 5, 1920), 'list');
        assert.equal(resolveEffectivePopoverLayout('auto-grid', 6, 1920), 'grid');
        assert.equal(resolveEffectivePopoverLayout('auto-grid', 8, 720), 'list');
    });

    it('summarizes overflow providers (6+) with preview names and semantic alert detection', () => {
        const overflow = [
            {id: 'openrouter', display_name: 'OpenRouter', primary_used_percent: 32, accounts: []},
            {id: 'copilot', display_name: 'GitHub Copilot', primary_used_percent: 40, accounts: []},
            {id: 'gemini', display_name: 'Gemini CLI', primary_used_percent: 12, accounts: []},
        ];
        const collapsed = summarizeOverflowProviders(overflow, false);
        assert.equal(collapsed.count, 3);
        assert.equal(collapsed.hasAlert, false);
        assert.equal(collapsed.alertSeverity, 'normal');
        assert.equal(collapsed.label, 'Show 3 more agents (OpenRouter · Copilot…) ▾');

        const expanded = summarizeOverflowProviders(overflow, true);
        assert.equal(expanded.label, 'Show fewer ▴');

        // Critical usage in 7th provider surfaces critical alert
        const withCritical = [
            overflow[0],
            {id: 'deepseek', display_name: 'DeepSeek', primary_used_percent: 94, accounts: []},
        ];
        const critSummary = summarizeOverflowProviders(withCritical, false);
        assert.equal(critSummary.hasAlert, true);
        assert.equal(critSummary.alertSeverity, 'critical');
        assert.equal(critSummary.label, 'Show 2 more agents (OpenRouter · DeepSeek) ▾');

        // Signed-out account or error state surfaces warning alert
        assert.equal(
            getProviderAlertSeverity({
                id: 'windsurf',
                accounts: [{id: 'w1', is_active: true, state: 'signed_out', buckets: []}],
            }),
            'warning'
        );
        assert.equal(
            getProviderAlertSeverity({
                id: 'openrouter',
                accounts: [{id: 'o1', is_active: true, state: 'error', error: 'HTTP 401', buckets: []}],
            }),
            'warning'
        );
    });
});

// ---------------------------------------------------------------------------
// 3. 7-Day Sparkline Normalization & 1-Click Clipboard Summary
// ---------------------------------------------------------------------------

describe('normalizeSparklinePoints & formatClipboardSummary', () => {
    const NOW = 1_760_000_000;

    it('returns null when fewer than 2 valid sparkline timestamps exist', () => {
        assert.equal(normalizeSparklinePoints(null), null);
        assert.equal(normalizeSparklinePoints([]), null);
        assert.equal(normalizeSparklinePoints([{ts: NOW, used: 20}]), null);
        assert.equal(
            normalizeSparklinePoints([
                {ts: NOW, used: 20},
                {ts: NOW, used: 35},
            ]),
            null
        );
    });

    it('sorts 7-day sparkline points, normalizes coordinates, and respects used vs remaining mode', () => {
        const raw = [
            {ts: NOW, used: 82},
            {ts: NOW - 6 * 86_400, used: 30},
            {ts: NOW - 3 * 86_400, used: 55},
        ];

        const usedSeries = normalizeSparklinePoints(raw, 'used');
        assert.ok(usedSeries);
        assert.equal(usedSeries.points.length, 3);
        assert.equal(usedSeries.points[0].used, 30);
        assert.equal(usedSeries.points[0].xNorm, 0);
        assert.ok(Math.abs(usedSeries.points[0].yNorm - 0.70) < 1e-6);
        assert.equal(usedSeries.points[2].used, 82);
        assert.equal(usedSeries.points[2].xNorm, 1);
        assert.ok(Math.abs(usedSeries.points[2].yNorm - 0.18) < 1e-6);
        assert.equal(usedSeries.deltaDisplay, 52);
        assert.equal(usedSeries.severity, 'warning');
        assert.equal(usedSeries.summaryText, '82% used (+52% / 6d)');

        const remSeries = normalizeSparklinePoints(raw, 'remaining');
        assert.ok(remSeries);
        assert.equal(remSeries.firstDisplay, 70);
        assert.equal(remSeries.latestDisplay, 18);
        assert.equal(remSeries.deltaDisplay, -52);
        assert.equal(remSeries.severity, 'warning');
        assert.equal(remSeries.summaryText, '18% left (-52% / 6d)');
    });

    it('formats plain-text clipboard summaries cleanly in both % used and % remaining modes', () => {
        assert.equal(
            formatClipboardSummary([], 'used', NOW),
            'AI Agent Usage: No active AI agents detected'
        );

        const providers = [
            {
                id: 'claude',
                display_name: 'Claude',
                plan_badge: 'Max 20x',
                accounts: [
                    {
                        id: 'claude:default',
                        is_active: true,
                        state: 'active_ok',
                        buckets: [
                            {
                                id: '5h',
                                label: '5-Hour',
                                used_percent: 42,
                                remaining_percent: 58,
                                resets_at_epoch: NOW + 7_200,
                            },
                            {
                                id: 'wk',
                                label: 'Weekly',
                                used_percent: 61,
                                remaining_percent: 39,
                                resets_at_epoch: NOW + 172_800,
                            },
                        ],
                    },
                ],
            },
            {
                id: 'openrouter',
                display_name: 'OpenRouter',
                accounts: [
                    {
                        id: 'openrouter:default',
                        is_active: true,
                        state: 'active_ok',
                        buckets: [],
                        credits: {label: 'Balance', balance_dollars: 18.4},
                    },
                ],
            },
            {
                id: 'deepseek',
                display_name: 'DeepSeek',
                accounts: [
                    {
                        id: 'deepseek:default',
                        is_active: true,
                        state: 'signed_out',
                        buckets: [],
                    },
                ],
            },
        ];

        const usedText = formatClipboardSummary(providers, 'used', NOW);
        assert.equal(
            usedText,
            [
                'AI Agent Usage (% used)',
                '- Claude (Max 20x): 5-Hour 42% used (resets in 2h 0m) · Weekly 61% used (resets in 2d 0h)',
                '- OpenRouter: Balance: $18.40 available',
                '- DeepSeek: Signed out',
            ].join('\n')
        );

        const remText = formatClipboardSummary(providers, 'remaining', NOW);
        assert.match(remText, /^AI Agent Usage \(% remaining\)/);
        assert.match(remText, /5-Hour 58% left/);
        assert.match(remText, /Weekly 39% left/);
    });
});

// ---------------------------------------------------------------------------
// 4. Burn-Rate / Pace Indicator (`inferBucketWindowSeconds` & `computeBucketPace`)
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

    it('classifies comfortable, on-track, elevated, and over-budget pace accurately', () => {
        assert.equal(
            computeBucketPace({short_label: '5h', used_percent: 20, resets_at_epoch: NOW + 17_460}, NOW),
            null
        );

        const comfortable = computeBucketPace({
            short_label: 'wk',
            used_percent: 19,
            resets_at_epoch: NOW + 453_600,
        }, NOW);
        assert.equal(comfortable?.state, 'under');
        assert.equal(comfortable?.severity, 'normal');

        const onTrack = computeBucketPace({
            short_label: '5h',
            used_percent: 44,
            resets_at_epoch: NOW + 9_000,
        }, NOW);
        assert.equal(onTrack?.state, 'on_track');

        const elevated = computeBucketPace({
            short_label: '24h',
            used_percent: 60,
            resets_at_epoch: NOW + 43_200,
        }, NOW);
        assert.equal(elevated?.state, 'elevated');
        assert.equal(elevated?.severity, 'warning');

        const over = computeBucketPace({
            short_label: 'premium',
            used_percent: 48,
            resets_at_epoch: NOW + 2_008_800,
        }, NOW);
        assert.equal(over?.state, 'over');
        assert.equal(over?.severity, 'critical');
    });
});

// ---------------------------------------------------------------------------
// 5. Report Normalization, Sparkline Preservation, Time-Decay & Panel Summary
// ---------------------------------------------------------------------------

describe('normalizeStatusReport, Labels & computePanelSummary', () => {
    const NOW = 1_760_000_000;

    it('formats bucket display labels across wk, 5h, 24h/daily, 31d, premium, and compound models', () => {
        assert.equal(formatBucketDisplayLabel({short_label: 'wk', label: 'wk'}), 'Weekly');
        assert.equal(formatBucketDisplayLabel({short_label: '5h', label: '5h'}), '5-Hour');
        assert.equal(formatBucketDisplayLabel({short_label: '24h', label: '24h'}), 'Daily');
        assert.equal(formatBucketDisplayLabel({short_label: '31d', label: '31d'}), 'Monthly');
        assert.equal(formatBucketDisplayLabel({short_label: 'premium', label: 'premium'}), 'Premium Requests');
        assert.equal(formatBucketDisplayLabel({short_label: 'Sonnet · wk', label: ''}), 'Sonnet · Weekly');
    });

    it('normalizes v2 reports across new providers, preserves sparklines, and decays expired buckets', () => {
        const raw = {
            schema_version: 2,
            updated_at_epoch: NOW - 30,
            providers: [
                {
                    id: 'openrouter',
                    active_account_id: 'openrouter:default',
                    accounts: [
                        {
                            id: 'openrouter:default',
                            label: 'API Key',
                            is_active: true,
                            state: 'active_ok',
                            sparkline: [
                                {ts: NOW - 86_400, used: 22},
                                {ts: NOW, used: 47.6},
                            ],
                            buckets: [
                                {
                                    id: '31d',
                                    short_label: '31d',
                                    is_primary: true,
                                    used_percent: 48,
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
                                    resets_at_epoch: NOW - 10,
                                },
                            ],
                        },
                    ],
                },
            ],
        };

        const report = normalizeStatusReport(raw, NOW);
        const openrouter = report.providers[0];
        assert.equal(openrouter.display_name, 'OpenRouter');
        assert.equal(openrouter.icon_name, 'provider-openrouter');
        assert.equal(openrouter.dashboard_url, 'https://openrouter.ai/activity');
        assert.deepEqual(openrouter.accounts[0].sparkline, [
            {ts: NOW - 86_400, used: 22},
            {ts: NOW, used: 48},
        ]);

        const gemini = report.providers[1];
        assert.equal(gemini.primary_used_percent, 0);
        assert.equal(gemini.primary_remaining_percent, 100);
    });

    it('computes panel summary and helper formatters cleanly', () => {
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
                    id: 'windsurf',
                    accounts: [{
                        id: 'w1',
                        is_active: true,
                        state: 'active_ok',
                        buckets: [{id: '31d', short_label: '31d', is_primary: true, used_percent: 84}],
                    }],
                },
            ],
        }, NOW);

        const usedSummary = computePanelSummary(report.providers, 'used', ' · ');
        assert.equal(usedSummary.severity, 'warning');
        assert.equal(usedSummary.fullText, 'Claude: 35% · Windsurf: 84%');

        assert.equal(clampPercent(-5), 0);
        assert.equal(clampPercent(120), 100);
        assert.equal(getSeverityForUsedPercent(80), 'warning');
        assert.equal(getSeverityForUsedPercent(90), 'critical');
        assert.equal(formatBucketMetric({used_percent: 25}, 'remaining').percentText, '75%');
        assert.equal(formatResetCountdown(NOW + 9_000, NOW), 'Resets in 2h 30m');
        assert.equal(formatRelativeUpdated(NOW - 125, NOW), 'Updated 2m ago');
        assert.equal(formatCreditsSummary({balance_dollars: 14.5}), 'Credits: $14.50 available');
        assert.equal(formatCompactAccountExtraNote({credits: {balance_dollars: 12}}, false), 'Credits: $12.00');
        assert.equal(groupBucketsForDetailedView([{id: 'a'}, {id: 'b'}]).length, 2);
        assert.equal(slugifyAccountId('  Work Account #2! '), 'work-account-2');
        assert.equal(buildConfigUpsertArgs({provider: 'openrouter', id: 'main', label: 'Main'})[3], 'openrouter');
    });
});

// ---------------------------------------------------------------------------
// 6. Smart Quota & Reset Desktop Notifications (`evaluateQuotaNotificationStep`)
// ---------------------------------------------------------------------------

describe('evaluateQuotaNotificationStep', () => {
    const NOW = 1_760_000_000;
    const WIN_1 = NOW + 172_800;
    const WIN_2 = WIN_1 + 604_800;

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

    it('handles hydration baseline, warning/critical once-per-window, reset re-arming, and hidden filtering', () => {
        const states = new Map();
        assert.deepEqual(
            evaluateQuotaNotificationStep(states, makeSingleBucketReport('claude', 94, WIN_1), {
                isInitialHydration: true,
                nowEpoch: NOW,
                notifyOnWarning: true,
                notifyOnCritical: true,
            }),
            []
        );

        const s2 = new Map();
        evaluateQuotaNotificationStep(s2, makeSingleBucketReport('codex', 70, WIN_1), {
            isInitialHydration: true,
            nowEpoch: NOW,
        });
        const warnOut = evaluateQuotaNotificationStep(s2, makeSingleBucketReport('codex', 85, WIN_1), {
            nowEpoch: NOW + 60,
            notifyOnWarning: true,
        });
        assert.equal(warnOut.length, 1);
        assert.equal(warnOut[0].kind, 'warning');

        const critOut = evaluateQuotaNotificationStep(s2, makeSingleBucketReport('codex', 92, WIN_1), {
            nowEpoch: NOW + 120,
            notifyOnCritical: true,
        });
        assert.equal(critOut.length, 1);
        assert.equal(critOut[0].kind, 'critical');

        const resetOut = evaluateQuotaNotificationStep(s2, makeSingleBucketReport('codex', 0, WIN_2), {
            nowEpoch: WIN_1 + 5,
            notifyOnReset: true,
        });
        assert.equal(resetOut.length, 1);
        assert.equal(resetOut[0].kind, 'reset');

        const sHidden = new Map();
        evaluateQuotaNotificationStep(sHidden, makeSingleBucketReport('openrouter', 50, WIN_1), {
            isInitialHydration: true,
            nowEpoch: NOW,
        });
        assert.deepEqual(
            evaluateQuotaNotificationStep(sHidden, makeSingleBucketReport('openrouter', 95, WIN_1), {
                nowEpoch: NOW + 60,
                hiddenProviderIds: DEFAULT_HIDDEN_PROVIDERS,
                notifyOnCritical: true,
            }),
            []
        );
    });
});

// ---------------------------------------------------------------------------
// 7. Shell Command Hardening (`posixShellQuote`, `tokenizeLoginCommand`, `buildTerminalArgv`)
// ---------------------------------------------------------------------------

describe('Shell Command Hardening & Injection Prevention', () => {
    it('quotes arbitrary strings safely and prevents shell command injection in /bin/sh', () => {
        assert.equal(posixShellQuote('claude'), "'claude'");
        assert.equal(posixShellQuote("a'b\0c"), "'a'\\''bc'");
        assert.deepEqual(
            tokenizeLoginCommand('"/opt/My AI Tools/claude" auth login'),
            ['/opt/My AI Tools/claude', 'auth', 'login']
        );

        const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'gas-shell-sec-'));
        try {
            const markerPath = path.join(tmpDir, 'injected-marker');
            const harmlessBin = path.join(tmpDir, 'mock-login');
            fs.writeFileSync(harmlessBin, '#!/bin/sh\nexit 0\n', {mode: 0o755});

            const payloads = [
                `"${harmlessBin}"; touch "${markerPath}"`,
                `"${harmlessBin}" $(touch "${markerPath}")`,
                `"${harmlessBin}" \`touch "${markerPath}"\``,
                `"${harmlessBin}" | touch "${markerPath}"`,
            ];

            for (const payload of payloads) {
                const argv = buildTerminalArgv('/usr/bin/kitty', payload, {homeDir: tmpDir});
                const nonInteractive = argv[3].replace(/;\s*printf[\s\S]*$/, '');
                try {
                    execFileSync('/bin/sh', ['-c', nonInteractive], {stdio: 'ignore', timeout: 3000});
                } catch (_e) {}
                assert.equal(fs.existsSync(markerPath), false);
            }
        } finally {
            fs.rmSync(tmpDir, {recursive: true, force: true});
        }
    });
});

// ---------------------------------------------------------------------------
// 8. High-Contrast WCAG Verification Across All 24 Themes + GNOME System Light
// ---------------------------------------------------------------------------

describe('High-Contrast WCAG Verification across all 24 themes + system light', () => {
    const cssText = fs.readFileSync(STYLESHEET_PATH, 'utf8');
    const cleanCss = cssText.replace(/\/\*[\s\S]*?\*\//g, '');

    it('enforces St CSS rules: strips host popup background, solidifies system theme, and avoids forbidden CSS', () => {
        assert.equal(/var\s*\(/.test(cleanCss), false, 'CSS must not use var()');
        assert.equal(/calc\s*\(/.test(cleanCss), false, 'CSS must not use calc()');
        assert.equal(/\bgap\s*:/.test(cleanCss), false, 'CSS must not use gap:');
        assert.equal(
            /\b(?:width|min-width|max-width)\s*:\s*\d+(?:\.\d+)?%/.test(cleanCss),
            false,
            'CSS must not use percentage widths'
        );
        assert.equal(/\bscale/.test(cleanCss), false, 'CSS must not use scale transforms');

        const hostBoxMatch = /\.gas-popup-menu-box\s*\{([^}]+)\}/.exec(cleanCss);
        assert.ok(hostBoxMatch, 'Missing .gas-popup-menu-box rule');
        assert.match(hostBoxMatch[1], /background-color:\s*transparent/);

        const sysRoot = extractThemeColorMap(cssText, '.gas-popover.theme-system').get('__root__');
        assert.ok(sysRoot);
        assert.notEqual(sysRoot['background-color'], 'transparent');

        const sysLightRoot = extractThemeColorMap(
            cssText,
            '.gas-popover.theme-system.is-light-surface'
        ).get('__root__');
        assert.ok(sysLightRoot);
        assert.notEqual(sysLightRoot['background-color'], 'transparent');
    });

    const surfaceSpecs = [
        ...THEME_PRESETS.map(p => ({
            id: p.id,
            label: p.label,
            isLight: p.isLight,
            selector: `.gas-popover.theme-${p.id}`,
            hostBg: p.bgHex,
        })),
        {
            id: 'system-light',
            label: 'GNOME System (Light)',
            isLight: true,
            selector: '.gas-popover.theme-system.is-light-surface',
            hostBg: SYSTEM_LIGHT_PALETTE.bgHex,
        },
    ];

    for (const spec of surfaceSpecs) {
        it(`achieves primary >= 10.0:1, secondary >= 5.8:1, and active >= 5.5:1 in ${spec.id}`, () => {
            const map = extractThemeColorMap(cssText, spec.selector);
            const root = map.get('__root__') ?? {};
            assert.ok(root['background-color'], `Missing background-color on ${spec.selector}`);
            assert.notEqual(root['background-color'], 'transparent');

            const hostBackdrop = parseCssColor(spec.hostBg);
            const popBg = compositeOver(parseCssColor(root['background-color']), hostBackdrop);

            const cardBgRaw = map.get('.gas-provider-card')?.['background-color'] ?? 'transparent';
            const cardBg = cardBgRaw === 'transparent'
                ? popBg
                : compositeOver(parseCssColor(cardBgRaw), popBg);

            const heroBgRaw = map.get('.gas-hero-card')?.['background-color'];
            assert.ok(heroBgRaw, `Missing .gas-hero-card background in ${spec.id}`);
            const heroBg = compositeOver(parseCssColor(heroBgRaw), popBg);

            const segBgRaw = map.get('.gas-segmented-track')?.['background-color'];
            const segBg = compositeOver(parseCssColor(segBgRaw), popBg);

            const activeSeg = map.get('.gas-segmented-btn.is-active');
            const activeSegBg = compositeOver(parseCssColor(activeSeg['background-color']), segBg);

            const activeThemeRow = map.get('.gas-theme-row.is-active');
            const activeRowBg = compositeOver(parseCssColor(activeThemeRow['background-color']), popBg);

            const selectedAccount = map.get('.gas-account-row.is-selected');
            const selectedAccountBg = compositeOver(parseCssColor(selectedAccount['background-color']), popBg);

            // 1. Primary text >= 10.0:1 across popover, provider card, and hero card
            assert.ok(
                contrastRatio(root.color, popBg) >= 10.0,
                `${spec.id} primary vs popBg < 10.0:1`
            );
            assert.ok(
                contrastRatio(map.get('.gas-provider-name')?.color, cardBg) >= 10.0,
                `${spec.id} provider-name vs cardBg < 10.0:1`
            );
            assert.ok(
                contrastRatio(map.get('.gas-hero-account-title')?.color, heroBg) >= 10.0,
                `${spec.id} hero-account-title vs heroBg < 10.0:1`
            );

            // 2. Secondary / muted text >= 5.8:1 across all chrome & labels
            const secondaryChecks = [
                ['header-subtitle', map.get('.gas-header-subtitle')?.color, popBg],
                ['plan-badge', map.get('.gas-plan-badge')?.color, cardBg],
                ['reset-label', map.get('.gas-reset-label')?.color, cardBg],
                ['bucket-full-label', map.get('.gas-bucket-full-label')?.color, cardBg],
                ['chevron', map.get('.gas-chevron')?.color, cardBg],
                ['status-note', map.get('.gas-status-note')?.color, cardBg],
                ['signed-out-hint', map.get('.gas-signed-out-hint')?.color, popBg],
                ['section-header', map.get('.gas-section-header')?.color, popBg],
                ['hero-caption-left', map.get('.gas-hero-caption-left')?.color, heroBg],
                ['sparkline-title', map.get('.gas-sparkline-title')?.color, heroBg],
                ['segmented-inactive', map.get('.gas-segmented-btn')?.color, segBg],
            ];
            for (const [label, fg, bg] of secondaryChecks) {
                assert.ok(fg, `Missing ${label} color in ${spec.id}`);
                const ratio = contrastRatio(fg, bg);
                assert.ok(
                    ratio >= 5.8,
                    `${spec.id} -> ${label} (${fg}) contrast ${ratio.toFixed(2)}:1 < 5.8:1`
                );
            }

            // 3. Active pills & selected rows >= 5.5:1
            assert.ok(
                contrastRatio(activeSeg.color, activeSegBg) >= 5.5,
                `${spec.id} active segmented btn contrast < 5.5:1`
            );
            assert.ok(
                contrastRatio(activeThemeRow.color, activeRowBg) >= 5.5,
                `${spec.id} active theme row contrast < 5.5:1`
            );
            assert.ok(
                contrastRatio(selectedAccount.color, selectedAccountBg) >= 5.5,
                `${spec.id} selected account row contrast < 5.5:1`
            );

            // 4. Light-theme avatar medallion contrast >= 10.0:1 for white brand logos
            if (spec.isLight) {
                const avatarBg = map.get('.gas-avatar-ring')?.['background-color'];
                assert.equal(avatarBg, '#222634', `${spec.id} must use #222634 dark slate avatar medallion`);
                assert.ok(
                    contrastRatio('#ffffff', avatarBg) >= 10.0,
                    `${spec.id} white logo vs .gas-avatar-ring < 10.0:1`
                );
            }

            // 5. Semantic warning/critical/error readability >= 4.8:1
            assert.ok(
                contrastRatio(map.get('.gas-bucket-pct.is-warning')?.color, cardBg) >= 4.8,
                `${spec.id} warning pct contrast < 4.8:1`
            );
            assert.ok(
                contrastRatio(map.get('.gas-bucket-pct.is-critical')?.color, cardBg) >= 4.8,
                `${spec.id} critical pct contrast < 4.8:1`
            );
            const errBanner = map.get('.gas-error-banner');
            const errBg = compositeOver(parseCssColor(errBanner['background-color']), popBg);
            assert.ok(
                contrastRatio(errBanner.color, errBg) >= 5.0,
                `${spec.id} error banner contrast < 5.0:1`
            );
        });
    }
});
