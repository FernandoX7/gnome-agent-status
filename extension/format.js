/**
 * Pure formatting, normalization, countdown, CLI-arg, and theme-palette helpers.
 * Zero GI/GNOME imports so this module can be imported safely by both GNOME Shell
 * (`extension.js`, `indicator.js`, `popover/*`), Libadwaita (`prefs.js`), and Node unit tests.
 */

export const CANONICAL_PROVIDERS = Object.freeze([
    Object.freeze({
        id: 'claude',
        displayName: 'Claude',
        iconName: 'provider-claude',
        shortCode: 'CC',
        defaultLoginCommand: 'claude auth login',
        locationHint: '~/.claude-work',
    }),
    Object.freeze({
        id: 'codex',
        displayName: 'Codex',
        iconName: 'provider-codex',
        shortCode: 'CX',
        defaultLoginCommand: 'codex login',
        locationHint: '~/.codex-work',
    }),
    Object.freeze({
        id: 'grok',
        displayName: 'Grok',
        iconName: 'provider-grok',
        shortCode: 'GK',
        defaultLoginCommand: 'grok login',
        locationHint: '~/.grok-work',
    }),
    Object.freeze({
        id: 'agy',
        displayName: 'Antigravity',
        iconName: 'provider-agy',
        shortCode: 'AG',
        defaultLoginCommand: 'agy auth login',
        locationHint: '~/.gemini/antigravity',
    }),
    Object.freeze({
        id: 'cursor',
        displayName: 'Cursor',
        iconName: 'provider-cursor',
        shortCode: 'CU',
        defaultLoginCommand: 'cursor',
        locationHint: '~/.config/Cursor/User/globalStorage/state.vscdb',
    }),
]);

export const PROVIDER_BY_ID = Object.freeze(
    Object.fromEntries(CANONICAL_PROVIDERS.map(p => [p.id, p]))
);

export const PANEL_MODES = Object.freeze([
    Object.freeze({id: 'icon', label: 'Minimal Icon'}),
    Object.freeze({id: 'compact-pills', label: 'Compact Pills'}),
    Object.freeze({id: 'full-text', label: 'Full Text'}),
]);

export const PANEL_BOXES = Object.freeze([
    Object.freeze({id: 'left', label: 'Left'}),
    Object.freeze({id: 'center', label: 'Center'}),
    Object.freeze({id: 'right', label: 'Right'}),
]);

export const DENSITY_MODES = Object.freeze([
    Object.freeze({id: 'detailed', label: 'Detailed'}),
    Object.freeze({id: 'compact', label: 'Compact'}),
]);

export const FONT_STYLES = Object.freeze([
    Object.freeze({id: 'system', label: 'System Tabular'}),
    Object.freeze({id: 'monospace', label: 'Monospace'}),
]);

export const METRIC_MODES = Object.freeze([
    Object.freeze({id: 'used', label: '% Used'}),
    Object.freeze({id: 'remaining', label: '% Remaining'}),
]);

export const TERMINAL_CANDIDATES = Object.freeze([
    'xdg-terminal-exec',
    'ptyxis',
    'kgx',
    'gnome-terminal',
    'x-terminal-emulator',
    'kitty',
    'alacritty',
    'ghostty',
    'foot',
]);

/**
 * Converts a `#rrggbb` hex string and alpha into a `[r, g, b, a]` float tuple (`0..1`) for Cairo.
 *
 * @param {string} hex
 * @param {number} [alpha=1.0]
 * @returns {[number, number, number, number]}
 */
export function hexToRgba(hex, alpha = 1.0) {
    const clean = String(hex || '').replace(/^#/, '');
    if (clean.length !== 6)
        return [0.4, 0.43, 0.49, alpha];
    const r = Number.parseInt(clean.slice(0, 2), 16) / 255;
    const g = Number.parseInt(clean.slice(2, 4), 16) / 255;
    const b = Number.parseInt(clean.slice(4, 6), 16) / 255;
    if (Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b))
        return [0.4, 0.43, 0.49, alpha];
    return [r, g, b, alpha];
}

export const THEME_PRESETS = Object.freeze([
    Object.freeze({
        id: 'midnight-slate',
        label: 'Midnight Slate',
        swatchHex: '#1e1f2f',
        trackRgba: hexToRgba('#171823', 0.96),
        heroTrackRgba: hexToRgba('#2c2d3c', 1.0),
        okFillRgba: hexToRgba('#676e7d', 1.0),
        warnFillRgba: hexToRgba('#e0af68', 1.0),
        critFillRgba: hexToRgba('#ff4443', 1.0),
    }),
    Object.freeze({
        id: 'system',
        label: 'GNOME System',
        swatchHex: '#3584e4',
        trackRgba: [0.5, 0.52, 0.58, 0.24],
        heroTrackRgba: [0.5, 0.52, 0.58, 0.30],
        okFillRgba: hexToRgba('#676e7d', 1.0),
        warnFillRgba: hexToRgba('#e0af68', 1.0),
        critFillRgba: hexToRgba('#ff4443', 1.0),
    }),
    Object.freeze({
        id: 'tokyo-night',
        label: 'Tokyo Night',
        swatchHex: '#1a1b26',
        trackRgba: hexToRgba('#14151e', 0.96),
        heroTrackRgba: hexToRgba('#24283b', 1.0),
        okFillRgba: hexToRgba('#676e7d', 1.0),
        warnFillRgba: hexToRgba('#e0af68', 1.0),
        critFillRgba: hexToRgba('#ff4443', 1.0),
    }),
    Object.freeze({
        id: 'catppuccin',
        label: 'Catppuccin Mocha',
        swatchHex: '#1e1e2e',
        trackRgba: hexToRgba('#14141f', 0.96),
        heroTrackRgba: hexToRgba('#313244', 1.0),
        okFillRgba: hexToRgba('#6c7086', 1.0),
        warnFillRgba: hexToRgba('#e0af68', 1.0),
        critFillRgba: hexToRgba('#ff4443', 1.0),
    }),
    Object.freeze({
        id: 'gruvbox',
        label: 'Gruvbox Dark',
        swatchHex: '#282828',
        trackRgba: hexToRgba('#1d2021', 0.96),
        heroTrackRgba: hexToRgba('#3c3836', 1.0),
        okFillRgba: hexToRgba('#928374', 1.0),
        warnFillRgba: hexToRgba('#d79921', 1.0),
        critFillRgba: hexToRgba('#fb4934', 1.0),
    }),
    Object.freeze({
        id: 'rose-pine',
        label: 'Rosé Pine',
        swatchHex: '#1f1d2e',
        trackRgba: hexToRgba('#161420', 0.96),
        heroTrackRgba: hexToRgba('#2a273f', 1.0),
        okFillRgba: hexToRgba('#6e6a86', 1.0),
        warnFillRgba: hexToRgba('#f6c177', 1.0),
        critFillRgba: hexToRgba('#eb6f92', 1.0),
    }),
    Object.freeze({
        id: 'nord',
        label: 'Nord',
        swatchHex: '#2e3440',
        trackRgba: hexToRgba('#222730', 0.96),
        heroTrackRgba: hexToRgba('#3b4252', 1.0),
        okFillRgba: hexToRgba('#676e7d', 1.0),
        warnFillRgba: hexToRgba('#ebcb8b', 1.0),
        critFillRgba: hexToRgba('#ff4443', 1.0),
    }),
    Object.freeze({
        id: 'everforest',
        label: 'Everforest Dark',
        swatchHex: '#2d353b',
        trackRgba: hexToRgba('#21272b', 0.96),
        heroTrackRgba: hexToRgba('#374145', 1.0),
        okFillRgba: hexToRgba('#7a8478', 1.0),
        warnFillRgba: hexToRgba('#dbbc7f', 1.0),
        critFillRgba: hexToRgba('#e67e80', 1.0),
    }),
]);

export const THEME_BY_ID = Object.freeze(
    Object.fromEntries(THEME_PRESETS.map(t => [t.id, t]))
);

/**
 * Resolves a theme preset safely, defaulting to `midnight-slate`.
 *
 * @param {string} themeId
 */
export function getThemePalette(themeId) {
    return THEME_BY_ID[themeId] ?? THEME_BY_ID['midnight-slate'];
}

/**
 * Clamps a percentage value to an integer in `[0, 100]`.
 *
 * @param {number} value
 * @returns {number}
 */
export function clampPercent(value) {
    if (typeof value !== 'number' || Number.isNaN(value))
        return 0;
    return Math.max(0, Math.min(100, Math.round(value)));
}

/**
 * Computes calm-technology severity strictly from `% used`:
 * `< 80%` => `'normal'`, `80..89%` => `'warning'`, `>= 90%` => `'critical'`.
 *
 * @param {number|null|undefined} usedPercent
 * @returns {'normal'|'warning'|'critical'}
 */
export function getSeverityForUsedPercent(usedPercent) {
    if (typeof usedPercent !== 'number' || Number.isNaN(usedPercent))
        return 'normal';
    if (usedPercent >= 90)
        return 'critical';
    if (usedPercent >= 80)
        return 'warning';
    return 'normal';
}

/**
 * Returns the `[r, g, b, a]` bar fill color for a given severity and theme.
 *
 * @param {'normal'|'warning'|'critical'} severity
 * @param {string} themeId
 * @returns {[number, number, number, number]}
 */
export function getBarFillRgba(severity, themeId = 'midnight-slate') {
    const palette = getThemePalette(themeId);
    if (severity === 'critical')
        return palette.critFillRgba;
    if (severity === 'warning')
        return palette.warnFillRgba;
    return palette.okFillRgba;
}

/**
 * Normalizes a bucket's metric values for `'used'` vs `'remaining'` display mode.
 * Severity always reflects exhaustion risk (`usedPercent >= 80` / `>= 90`).
 *
 * @param {object} bucket
 * @param {'used'|'remaining'} [metricMode='used']
 */
export function formatBucketMetric(bucket, metricMode = 'used') {
    const usedPercent = clampPercent(bucket?.used_percent ?? 0);
    const remainingPercent = typeof bucket?.remaining_percent === 'number'
        ? clampPercent(bucket.remaining_percent)
        : Math.max(0, 100 - usedPercent);
    const isRemaining = metricMode === 'remaining';
    const displayPercent = isRemaining ? remainingPercent : usedPercent;
    const fillFraction = displayPercent / 100;
    const severity = getSeverityForUsedPercent(usedPercent);

    return {
        usedPercent,
        remainingPercent,
        displayPercent,
        fillFraction,
        percentText: `${displayPercent}%`,
        heroCaption: isRemaining ? `${displayPercent}% left` : `${displayPercent}% used`,
        severity,
    };
}

/**
 * Cleans raw bucket labels so cognitive "Limit Remaining" wording never conflicts
 * with normalized `% used` / `% remaining` gauges.
 *
 * @param {string} label
 * @returns {string}
 */
export function normalizeBucketLabel(label) {
    if (!label || typeof label !== 'string')
        return '';
    return label
        .replace(/\s*Limit\s+Remaining\b/gi, '')
        .replace(/\bFive\s+Hour\b/gi, '5h')
        .replace(/\bGemini\s+Models\b/gi, 'Gemini')
        .replace(/\bClaude\s+and\s+GPT\s+models\b/gi, 'Claude & GPT')
        .replace(/\s+/g, ' ')
        .trim();
}

/**
 * Groups ordered buckets for the Detailed main popover card:
 * - `layout_hint === 'inline_pair'` consecutive items pair 2-up on one line
 * - `layout_hint === 'full_row'` items take a dedicated full-width line
 *
 * @param {Array<object>} buckets
 * @returns {Array<{kind: 'pair', items: Array<object>} | {kind: 'full', item: object}>}
 */
export function groupBucketsForDetailedView(buckets) {
    if (!Array.isArray(buckets) || buckets.length === 0)
        return [];

    const groups = [];
    let pendingPair = [];

    const flushPair = () => {
        if (pendingPair.length > 0) {
            groups.push({kind: 'pair', items: pendingPair});
            pendingPair = [];
        }
    };

    for (const bucket of buckets) {
        if (!bucket)
            continue;
        if (bucket.layout_hint === 'full_row') {
            flushPair();
            groups.push({kind: 'full', item: bucket});
        } else {
            pendingPair.push(bucket);
            if (pendingPair.length === 2)
                flushPair();
        }
    }
    flushPair();
    return groups;
}

/**
 * Formats an epoch reset timestamp as a live countdown (`Resets in 1d 13h`, `14h 44m`, `12m`).
 *
 * @param {number|null|undefined} resetEpoch
 * @param {number} nowEpoch
 * @param {{prefix?: boolean}} [options]
 * @returns {string}
 */
export function formatResetCountdown(resetEpoch, nowEpoch, {prefix = true} = {}) {
    if (typeof resetEpoch !== 'number' || !Number.isFinite(resetEpoch) || resetEpoch <= 0)
        return '';

    const diff = Math.floor(resetEpoch - nowEpoch);
    if (diff <= 0)
        return prefix ? 'Resets now' : 'now';

    const days = Math.floor(diff / 86400);
    const hours = Math.floor((diff % 86400) / 3600);
    const minutes = Math.floor((diff % 3600) / 60);

    let core;
    if (days > 0)
        core = `${days}d ${hours}h`;
    else if (hours > 0)
        core = `${hours}h ${minutes}m`;
    else
        core = `${Math.max(1, minutes)}m`;

    return prefix ? `Resets in ${core}` : core;
}

/**
 * Formats an epoch update timestamp into a calm relative freshness label (`Updated just now`, `Updated 4m ago`).
 *
 * @param {number|null|undefined} updatedEpoch
 * @param {number} nowEpoch
 * @returns {string}
 */
export function formatRelativeUpdated(updatedEpoch, nowEpoch) {
    if (typeof updatedEpoch !== 'number' || !Number.isFinite(updatedEpoch) || updatedEpoch <= 0)
        return 'Not updated yet';

    const age = Math.max(0, Math.floor(nowEpoch - updatedEpoch));
    if (age < 45)
        return 'Updated just now';
    if (age < 3600)
        return `Updated ${Math.floor(age / 60)}m ago`;
    if (age < 86400)
        return `Updated ${Math.floor(age / 3600)}h ago`;
    return `Updated ${Math.floor(age / 86400)}d ago`;
}

/**
 * Formats optional account credits/extra-usage metadata into a single concise caption.
 *
 * @param {object|null|undefined} credits
 * @returns {string}
 */
export function formatCreditsSummary(credits) {
    if (!credits || typeof credits !== 'object')
        return '';

    const label = credits.label || 'Credits';
    const hasUsed = typeof credits.used_dollars === 'number' && Number.isFinite(credits.used_dollars);
    const hasLimit = typeof credits.limit_dollars === 'number' && Number.isFinite(credits.limit_dollars);
    const hasBalance = typeof credits.balance_dollars === 'number' && Number.isFinite(credits.balance_dollars);
    const hasPct = typeof credits.used_percent === 'number' && Number.isFinite(credits.used_percent);

    if (hasUsed && hasLimit) {
        const suffix = hasPct ? ` (${clampPercent(credits.used_percent)}%)` : '';
        return `${label}: $${credits.used_dollars.toFixed(2)} / $${credits.limit_dollars.toFixed(2)}${suffix}`;
    }
    if (hasBalance)
        return `${label}: $${credits.balance_dollars.toFixed(2)} available`;
    if (hasUsed)
        return `${label}: $${credits.used_dollars.toFixed(2)} used`;
    if (hasPct)
        return `${label}: ${clampPercent(credits.used_percent)}% used`;
    return '';
}

/**
 * Resolves the active AccountReport for a ProviderReport.
 *
 * @param {object} provider
 * @returns {object|null}
 */
export function getActiveAccount(provider) {
    const accounts = Array.isArray(provider?.accounts) ? provider.accounts : [];
    if (accounts.length === 0)
        return null;
    if (provider.active_account_id) {
        const byId = accounts.find(a => a && a.id === provider.active_account_id);
        if (byId)
            return byId;
    }
    return accounts.find(a => a?.is_active) ?? accounts[0];
}

/**
 * Normalizes a raw `StatusReport` (v2 or legacy v1) and applies live passive reset decay
 * (`nowEpoch >= resets_at_epoch => used_percent = 0, remaining_percent = 100`).
 *
 * @param {object|null|undefined} raw
 * @param {number} nowEpoch
 * @returns {object}
 */
export function normalizeStatusReport(raw, nowEpoch = Math.floor(Date.now() / 1000)) {
    if (!raw || typeof raw !== 'object') {
        return {
            schema_version: 2,
            label: '',
            status: 'ok',
            updated_at_epoch: 0,
            summary: {
                max_used_percent: 0,
                active_provider_count: 0,
                has_auth_error: false,
            },
            providers: [],
            agents: [],
        };
    }

    let rawProviders = Array.isArray(raw.providers) ? raw.providers : null;

    // Backwards-compatible upgrade if a v1 report (`agents` without `providers`) is received
    if (!rawProviders && Array.isArray(raw.agents)) {
        rawProviders = raw.agents.map((agent, idx) => {
            const id = String(agent.agent_type || `agent-${idx}`).toLowerCase();
            const meta = PROVIDER_BY_ID[id];
            const hasMetric = typeof agent.used_percent === 'number';
            const used = hasMetric ? clampPercent(agent.used_percent) : null;
            const rem = hasMetric ? Math.max(0, 100 - used) : null;
            return {
                id,
                display_name: agent.name || meta?.displayName || id,
                icon_name: meta?.iconName || 'agent-status-symbolic',
                plan_badge: null,
                dashboard_url: null,
                active_account_id: `${id}:default`,
                primary_used_percent: used,
                primary_remaining_percent: rem,
                primary_reset_epoch: null,
                primary_reset_iso: agent.reset_info || null,
                accounts: [
                    {
                        id: `${id}:default`,
                        label: agent.name || meta?.displayName || id,
                        source_label: null,
                        is_system_default: true,
                        is_active: true,
                        plan_tier: null,
                        state: agent.error ? 'error' : 'active_ok',
                        is_stale: false,
                        status_note: agent.warning || null,
                        error: agent.error || null,
                        login_command: meta?.defaultLoginCommand || null,
                        updated_at_epoch: raw.updated_at_epoch || 0,
                        buckets: hasMetric
                            ? [
                                {
                                    id: 'wk',
                                    label: 'Weekly',
                                    short_label: 'wk',
                                    layout_hint: 'inline_pair',
                                    is_primary: true,
                                    used_percent: used,
                                    remaining_percent: rem,
                                    resets_at_epoch: null,
                                    resets_at_iso: agent.reset_info || null,
                                },
                            ]
                            : [],
                        credits: null,
                    },
                ],
            };
        });
    }

    const providers = (rawProviders ?? []).map(prov => {
        const meta = PROVIDER_BY_ID[prov?.id];
        const accounts = (Array.isArray(prov?.accounts) ? prov.accounts : []).map(acc => {
            const buckets = (Array.isArray(acc?.buckets) ? acc.buckets : []).map(b => {
                const expired =
                    typeof b?.resets_at_epoch === 'number' &&
                    b.resets_at_epoch > 0 &&
                    nowEpoch >= b.resets_at_epoch;
                const used = expired ? 0 : clampPercent(b?.used_percent ?? 0);
                const rem = expired
                    ? 100
                    : typeof b?.remaining_percent === 'number'
                        ? clampPercent(b.remaining_percent)
                        : Math.max(0, 100 - used);
                return {
                    ...b,
                    label: normalizeBucketLabel(b?.label || b?.short_label || 'Usage'),
                    short_label: b?.short_label || 'wk',
                    layout_hint: b?.layout_hint === 'full_row' ? 'full_row' : 'inline_pair',
                    used_percent: used,
                    remaining_percent: rem,
                };
            });

            return {
                ...acc,
                buckets,
                login_command: acc?.login_command || meta?.defaultLoginCommand || null,
            };
        });

        const normalizedProvider = {
            ...prov,
            display_name: prov?.display_name || meta?.displayName || prov?.id || 'Agent',
            icon_name: prov?.icon_name || meta?.iconName || 'agent-status-symbolic',
            accounts,
        };

        const activeAcc = getActiveAccount(normalizedProvider);
        const primaryBucket =
            activeAcc?.buckets?.find(b => b.is_primary) ?? activeAcc?.buckets?.[0] ?? null;

        if (primaryBucket) {
            normalizedProvider.primary_used_percent = primaryBucket.used_percent;
            normalizedProvider.primary_remaining_percent = primaryBucket.remaining_percent;
            if (primaryBucket.resets_at_epoch)
                normalizedProvider.primary_reset_epoch = primaryBucket.resets_at_epoch;
        } else if (
            typeof normalizedProvider.primary_reset_epoch === 'number' &&
            normalizedProvider.primary_reset_epoch > 0 &&
            nowEpoch >= normalizedProvider.primary_reset_epoch
        ) {
            normalizedProvider.primary_used_percent = 0;
            normalizedProvider.primary_remaining_percent = 100;
        }

        return normalizedProvider;
    });

    let maxUsed = 0;
    let hasAuthError = false;
    for (const prov of providers) {
        if (typeof prov.primary_used_percent === 'number')
            maxUsed = Math.max(maxUsed, prov.primary_used_percent);
        const activeAcc = getActiveAccount(prov);
        if (activeAcc?.state === 'signed_out')
            hasAuthError = true;
    }

    return {
        schema_version: 2,
        label: typeof raw.label === 'string' ? raw.label : '',
        status: raw.status || 'ok',
        updated_at_epoch: typeof raw.updated_at_epoch === 'number' ? raw.updated_at_epoch : 0,
        summary: {
            max_used_percent: raw.summary?.max_used_percent ?? maxUsed,
            active_provider_count: raw.summary?.active_provider_count ?? providers.length,
            has_auth_error: raw.summary?.has_auth_error ?? hasAuthError,
        },
        providers,
        agents: Array.isArray(raw.agents) ? raw.agents : [],
    };
}

/**
 * Filters a provider list against the user's `hidden-providers` setting.
 *
 * @param {Array<object>} providers
 * @param {Array<string>} hiddenProviderIds
 * @returns {Array<object>}
 */
export function filterVisibleProviders(providers, hiddenProviderIds = []) {
    if (!Array.isArray(providers) || providers.length === 0)
        return [];
    if (!Array.isArray(hiddenProviderIds) || hiddenProviderIds.length === 0)
        return providers;
    const hidden = new Set(hiddenProviderIds);
    return providers.filter(p => p && !hidden.has(p.id));
}

/**
 * Computes top-bar summary data (severity, compact pills, and full-text label)
 * from the currently visible providers.
 *
 * @param {Array<object>} visibleProviders
 * @param {'used'|'remaining'} [metricMode='used']
 * @param {string} [separator=' | ']
 */
export function computePanelSummary(visibleProviders, metricMode = 'used', separator = ' | ') {
    const providers = Array.isArray(visibleProviders) ? visibleProviders : [];
    let maxUsedPercent = 0;
    let hasAuthError = false;
    const pills = [];
    const textSegments = [];

    for (const prov of providers) {
        const activeAcc = getActiveAccount(prov);
        if (activeAcc?.state === 'signed_out')
            hasAuthError = true;

        const hasPrimary = typeof prov.primary_used_percent === 'number';
        if (hasPrimary) {
            const used = clampPercent(prov.primary_used_percent);
            const rem = typeof prov.primary_remaining_percent === 'number'
                ? clampPercent(prov.primary_remaining_percent)
                : Math.max(0, 100 - used);
            const displayPercent = metricMode === 'remaining' ? rem : used;
            const itemSeverity = getSeverityForUsedPercent(used);
            maxUsedPercent = Math.max(maxUsedPercent, used);

            pills.push({
                id: prov.id,
                displayName: prov.display_name,
                iconName: prov.icon_name,
                usedPercent: used,
                displayPercent,
                percentText: `${displayPercent}%`,
                severity: itemSeverity,
            });
            textSegments.push(`${prov.display_name}: ${displayPercent}%`);
        } else if (activeAcc?.state === 'signed_out') {
            pills.push({
                id: prov.id,
                displayName: prov.display_name,
                iconName: prov.icon_name,
                usedPercent: null,
                displayPercent: null,
                percentText: '!',
                severity: 'critical',
            });
            textSegments.push(`${prov.display_name}: AUTH`);
        }
    }

    let severity = getSeverityForUsedPercent(maxUsedPercent);
    if (hasAuthError && severity === 'normal')
        severity = 'warning';
    if (hasAuthError && providers.length > 0 && pills.every(p => p.usedPercent === null))
        severity = 'critical';

    const fullText = textSegments.length > 0 ? textSegments.join(separator) : 'Agents: —';

    return {
        maxUsedPercent,
        activeCount: providers.length,
        hasAuthError,
        severity,
        pills,
        fullText,
    };
}

/**
 * Normalizes an arbitrary string into a clean account slug (`[a-z0-9_-]`).
 *
 * @param {string} raw
 * @returns {string}
 */
export function slugifyAccountId(raw) {
    const cleaned = String(raw || '')
        .trim()
        .toLowerCase()
        .replace(/[^a-z0-9_-]+/g, '-')
        .replace(/^-+|-+$/g, '');
    return cleaned || 'custom';
}

/**
 * Builds exact CLI subcommand arguments for `gnome-agent-status config upsert-account`.
 *
 * @param {{
 *   provider: string,
 *   id: string,
 *   label: string,
 *   path?: string|null,
 *   binary?: string|null,
 *   pollMode?: string|null,
 *   switchCommand?: string|null
 * }} spec
 * @returns {string[]}
 */
export function buildConfigUpsertArgs(spec) {
    const provider = String(spec.provider || 'claude').trim();
    const slug = slugifyAccountId(spec.id || spec.label || 'custom');
    const label = String(spec.label || slug).trim();

    const args = [
        'config',
        'upsert-account',
        '--provider',
        provider,
        '--id',
        slug,
        '--label',
        label,
    ];

    if (spec.path && String(spec.path).trim() !== '')
        args.push('--path', String(spec.path).trim());
    if (spec.binary && String(spec.binary).trim() !== '')
        args.push('--binary', String(spec.binary).trim());
    if (spec.pollMode === 'passive' || spec.pollMode === 'active')
        args.push('--poll-mode', spec.pollMode);
    if (spec.switchCommand && String(spec.switchCommand).trim() !== '')
        args.push('--switch-command', String(spec.switchCommand).trim());

    return args;
}

/**
 * Builds terminal invocation argv for launching an interactive `login_command`
 * across standard Linux terminal emulators.
 *
 * @param {string} terminalBinary
 * @param {string} loginCommand
 * @returns {string[]}
 */
export function buildTerminalArgv(terminalBinary, loginCommand) {
    const baseName = String(terminalBinary || '').split('/').pop();
    const rawCmd = String(loginCommand || '').trim();
    let safeCmd = rawCmd || 'echo "No login command configured"';
    if (safeCmd === 'agy auth login') {
        safeCmd = 'if command -v agy >/dev/null 2>&1; then agy auth login; elif command -v antigravity >/dev/null 2>&1; then antigravity; else agy auth login; fi';
    }
    const pathBootstrap = 'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.grok/bin:$PATH"';
    const interactiveScript = `${pathBootstrap}; ${safeCmd}; printf '\\nPress Enter to close...'; read -r _`;

    switch (baseName) {
    case 'ptyxis':
        return [terminalBinary, '--new-window', '--', 'sh', '-c', interactiveScript];
    case 'kgx':
    case 'gnome-terminal':
    case 'xdg-terminal-exec':
        return [terminalBinary, '--', 'sh', '-c', interactiveScript];
    case 'alacritty':
    case 'ghostty':
    case 'x-terminal-emulator':
        return [terminalBinary, '-e', 'sh', '-c', interactiveScript];
    case 'kitty':
    case 'foot':
    default:
        return [terminalBinary, 'sh', '-c', interactiveScript];
    }
}
