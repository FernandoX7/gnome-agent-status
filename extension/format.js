/**
 * Pure formatting, normalization, countdown, sparkline, clipboard, CLI-arg, and theme-palette helpers.
 * Zero GI/GNOME imports so this module can be imported safely by GNOME Shell
 * (`extension.js`, `indicator.js`, `popover/*`), Libadwaita (`prefs.js`), and Node unit tests.
 */

export const DEFAULT_PROVIDER_ORDER = Object.freeze([
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

export const DEFAULT_HIDDEN_PROVIDERS = Object.freeze([
    'openrouter',
    'copilot',
    'gemini',
    'windsurf',
    'deepseek',
]);

export const CANONICAL_PROVIDERS = Object.freeze([
    Object.freeze({
        id: 'claude',
        displayName: 'Claude',
        iconName: 'provider-claude',
        shortCode: 'CC',
        defaultLoginCommand: 'claude auth login',
        locationHint: '~/.claude-work',
        dashboardUrl: 'https://claude.ai/settings/usage',
    }),
    Object.freeze({
        id: 'codex',
        displayName: 'Codex',
        iconName: 'provider-codex',
        shortCode: 'CX',
        defaultLoginCommand: 'codex login',
        locationHint: '~/.codex-work',
        dashboardUrl: 'https://chatgpt.com/codex/settings/usage',
    }),
    Object.freeze({
        id: 'grok',
        displayName: 'Grok',
        iconName: 'provider-grok',
        shortCode: 'GK',
        defaultLoginCommand: 'grok login',
        locationHint: '~/.grok-work',
        dashboardUrl: 'https://grok.com',
    }),
    Object.freeze({
        id: 'cursor',
        displayName: 'Cursor',
        iconName: 'provider-cursor',
        shortCode: 'CU',
        defaultLoginCommand: 'cursor',
        locationHint: '~/.config/Cursor/User/globalStorage/state.vscdb',
        dashboardUrl: 'https://www.cursor.com/dashboard',
    }),
    Object.freeze({
        id: 'agy',
        displayName: 'Antigravity',
        iconName: 'provider-agy',
        shortCode: 'AG',
        defaultLoginCommand: 'agy auth login',
        locationHint: '~/.gemini/antigravity',
        dashboardUrl: 'https://antigravity.google/',
    }),
    Object.freeze({
        id: 'openrouter',
        displayName: 'OpenRouter',
        iconName: 'provider-openrouter',
        shortCode: 'OR',
        defaultLoginCommand: 'openrouter',
        locationHint: '~/.config/openrouter/api_key',
        dashboardUrl: 'https://openrouter.ai/activity',
    }),
    Object.freeze({
        id: 'copilot',
        displayName: 'GitHub Copilot',
        iconName: 'provider-copilot',
        shortCode: 'CP',
        defaultLoginCommand: 'gh auth login',
        locationHint: '~/.config/github-copilot/hosts.json',
        dashboardUrl: 'https://github.com/settings/copilot',
    }),
    Object.freeze({
        id: 'gemini',
        displayName: 'Gemini CLI',
        iconName: 'provider-gemini',
        shortCode: 'GM',
        defaultLoginCommand: 'gemini',
        locationHint: '~/.gemini/oauth_creds.json',
        dashboardUrl: 'https://aistudio.google.com/',
    }),
    Object.freeze({
        id: 'windsurf',
        displayName: 'Windsurf',
        iconName: 'provider-windsurf',
        shortCode: 'WS',
        defaultLoginCommand: 'windsurf',
        locationHint: '~/.config/Windsurf/User/globalStorage/state.vscdb',
        dashboardUrl: 'https://windsurf.com/subscription',
    }),
    Object.freeze({
        id: 'deepseek',
        displayName: 'DeepSeek',
        iconName: 'provider-deepseek',
        shortCode: 'DS',
        defaultLoginCommand: 'deepseek',
        locationHint: '~/.config/deepseek/api_key',
        dashboardUrl: 'https://platform.deepseek.com/usage',
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

export const POPOVER_LAYOUTS = Object.freeze([
    Object.freeze({id: 'list', label: 'Compact List'}),
    Object.freeze({id: 'auto-grid', label: 'Auto 2-Column'}),
    Object.freeze({id: 'grid', label: '2-Column Wide'}),
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

export const THEME_CATEGORIES = Object.freeze([
    Object.freeze({id: 'all', label: 'All'}),
    Object.freeze({id: 'dark', label: 'Dark'}),
    Object.freeze({id: 'light', label: 'Light'}),
    Object.freeze({id: 'glass', label: 'Glass'}),
]);

export const QUICK_THEME_IDS = Object.freeze([
    'midnight-slate',
    'liquid-glass',
    'catppuccin-latte',
    'tokyo-night',
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

function makeThemePreset({
    id,
    label,
    category,
    isLight,
    swatchHex,
    accentHex,
    bgHex,
    fgHex,
    mutedHex,
    trackHex,
    trackAlpha = 0.96,
    heroTrackHex,
    heroTrackAlpha = 1.0,
    okFillHex,
    warnFillHex,
    critFillHex,
}) {
    return Object.freeze({
        id,
        label,
        category,
        isLight: Boolean(isLight),
        swatchHex,
        accentHex,
        bgHex,
        fgHex,
        mutedHex,
        trackRgba: Object.freeze(hexToRgba(trackHex, trackAlpha)),
        heroTrackRgba: Object.freeze(hexToRgba(heroTrackHex, heroTrackAlpha)),
        okFillRgba: Object.freeze(hexToRgba(okFillHex, 1.0)),
        warnFillRgba: Object.freeze(hexToRgba(warnFillHex, 1.0)),
        critFillRgba: Object.freeze(hexToRgba(critFillHex, 1.0)),
    });
}

export const THEME_PRESETS = Object.freeze([
    // --- Glass & Adaptive (3) ---
    makeThemePreset({
        id: 'system',
        label: 'GNOME System',
        category: 'glass',
        isLight: false,
        swatchHex: '#3584e4',
        accentHex: '#62a0ea',
        bgHex: '#242429',
        fgHex: '#f4f6fb',
        mutedHex: '#b2b8c9',
        trackHex: '#3c3f4d',
        heroTrackHex: '#373a47',
        okFillHex: '#8d96ad',
        warnFillHex: '#e5a952',
        critFillHex: '#ff4d52',
    }),
    makeThemePreset({
        id: 'liquid-glass',
        label: 'Liquid Glass',
        category: 'glass',
        isLight: false,
        swatchHex: '#28364e',
        accentHex: '#64d2ff',
        bgHex: '#181d2a',
        fgHex: '#f5f8ff',
        mutedHex: '#c0cce2',
        trackHex: '#475673',
        trackAlpha: 0.72,
        heroTrackHex: '#3f4d68',
        heroTrackAlpha: 0.82,
        okFillHex: '#64d2ff',
        warnFillHex: '#ffd166',
        critFillHex: '#ff5c64',
    }),
    makeThemePreset({
        id: 'liquid-glass-light',
        label: 'Liquid Glass Light',
        category: 'glass',
        isLight: true,
        swatchHex: '#dce8fa',
        accentHex: '#0066cc',
        bgHex: '#f4f7fd',
        fgHex: '#0f141f',
        mutedHex: '#3b4559',
        trackHex: '#c5d1e4',
        trackAlpha: 0.90,
        heroTrackHex: '#bcc9df',
        heroTrackAlpha: 0.95,
        okFillHex: '#0066cc',
        warnFillHex: '#c76a00',
        critFillHex: '#d1242f',
    }),

    // --- Light Palettes (5) ---
    makeThemePreset({
        id: 'catppuccin-latte',
        label: 'Catppuccin Latte',
        category: 'light',
        isLight: true,
        swatchHex: '#eff1f5',
        accentHex: '#1e66f5',
        bgHex: '#eff1f5',
        fgHex: '#191b28',
        mutedHex: '#43475c',
        trackHex: '#ccd0da',
        heroTrackHex: '#c2c7d4',
        okFillHex: '#1e66f5',
        warnFillHex: '#c96a00',
        critFillHex: '#d20f39',
    }),
    makeThemePreset({
        id: 'flexoki-light',
        label: 'Flexoki Light',
        category: 'light',
        isLight: true,
        swatchHex: '#fffcf0',
        accentHex: '#205ea6',
        bgHex: '#fffcf0',
        fgHex: '#100f0f',
        mutedHex: '#4b4944',
        trackHex: '#dad8ce',
        heroTrackHex: '#cecdc3',
        okFillHex: '#205ea6',
        warnFillHex: '#bc6700',
        critFillHex: '#af3029',
    }),
    makeThemePreset({
        id: 'rose-pine-dawn',
        label: 'Rosé Pine Dawn',
        category: 'light',
        isLight: true,
        swatchHex: '#faf4ed',
        accentHex: '#907aa9',
        bgHex: '#faf4ed',
        fgHex: '#262138',
        mutedHex: '#575279',
        trackHex: '#dfd5cc',
        heroTrackHex: '#d5c9bf',
        okFillHex: '#286983',
        warnFillHex: '#c96f18',
        critFillHex: '#b43356',
    }),
    makeThemePreset({
        id: 'daylight-white',
        label: 'Pure Daylight',
        category: 'light',
        isLight: true,
        swatchHex: '#ffffff',
        accentHex: '#0969da',
        bgHex: '#ffffff',
        fgHex: '#0d1117',
        mutedHex: '#424a53',
        trackHex: '#d8dee4',
        heroTrackHex: '#cfd7df',
        okFillHex: '#0969da',
        warnFillHex: '#bf6700',
        critFillHex: '#cf222e',
    }),
    makeThemePreset({
        id: 'lumon',
        label: 'Lumon Ice',
        category: 'light',
        isLight: true,
        swatchHex: '#edf4f8',
        accentHex: '#0e6994',
        bgHex: '#edf4f8',
        fgHex: '#0d1b26',
        mutedHex: '#354b5c',
        trackHex: '#c6dae6',
        heroTrackHex: '#bad0de',
        okFillHex: '#0e6994',
        warnFillHex: '#c46a00',
        critFillHex: '#c92a35',
    }),

    // --- Dark Palettes (16) ---
    makeThemePreset({
        id: 'midnight-slate',
        label: 'Midnight Slate',
        category: 'dark',
        isLight: false,
        swatchHex: '#1e1f2f',
        accentHex: '#7e97d6',
        bgHex: '#1e1f2f',
        fgHex: '#eef2ff',
        mutedHex: '#a7b0cc',
        trackHex: '#343852',
        heroTrackHex: '#30344d',
        okFillHex: '#8c97b5',
        warnFillHex: '#e0af68',
        critFillHex: '#ff4d52',
    }),
    makeThemePreset({
        id: 'tokyo-night',
        label: 'Tokyo Night',
        category: 'dark',
        isLight: false,
        swatchHex: '#1a1b26',
        accentHex: '#7aa2f7',
        bgHex: '#1a1b26',
        fgHex: '#dde5ff',
        mutedHex: '#a3aed2',
        trackHex: '#313750',
        heroTrackHex: '#2d334a',
        okFillHex: '#7aa2f7',
        warnFillHex: '#e0af68',
        critFillHex: '#f7768e',
    }),
    makeThemePreset({
        id: 'catppuccin',
        label: 'Catppuccin Mocha',
        category: 'dark',
        isLight: false,
        swatchHex: '#1e1e2e',
        accentHex: '#89b4fa',
        bgHex: '#1e1e2e',
        fgHex: '#e6ecff',
        mutedHex: '#aeb5d1',
        trackHex: '#36384d',
        heroTrackHex: '#33354a',
        okFillHex: '#89b4fa',
        warnFillHex: '#f9e2af',
        critFillHex: '#f38ba8',
    }),
    makeThemePreset({
        id: 'everforest',
        label: 'Everforest',
        category: 'dark',
        isLight: false,
        swatchHex: '#272e33',
        accentHex: '#a7c080',
        bgHex: '#272e33',
        fgHex: '#f0e9d6',
        mutedHex: '#b4c2b6',
        trackHex: '#3d484d',
        heroTrackHex: '#394449',
        okFillHex: '#a7c080',
        warnFillHex: '#dbbc7f',
        critFillHex: '#e67e80',
    }),
    makeThemePreset({
        id: 'gruvbox',
        label: 'Gruvbox',
        category: 'dark',
        isLight: false,
        swatchHex: '#282828',
        accentHex: '#83a598',
        bgHex: '#282828',
        fgHex: '#fbf1c7',
        mutedHex: '#c6b8a2',
        trackHex: '#45403d',
        heroTrackHex: '#403b38',
        okFillHex: '#83a598',
        warnFillHex: '#fabd2f',
        critFillHex: '#fb4934',
    }),
    makeThemePreset({
        id: 'kanagawa',
        label: 'Kanagawa',
        category: 'dark',
        isLight: false,
        swatchHex: '#1f1f28',
        accentHex: '#7e9cd8',
        bgHex: '#1f1f28',
        fgHex: '#f2ecda',
        mutedHex: '#b5b2a4',
        trackHex: '#363646',
        heroTrackHex: '#313140',
        okFillHex: '#7e9cd8',
        warnFillHex: '#dca561',
        critFillHex: '#e82424',
    }),
    makeThemePreset({
        id: 'matte-black',
        label: 'Matte Black',
        category: 'dark',
        isLight: false,
        swatchHex: '#121212',
        accentHex: '#e6e6e6',
        bgHex: '#121212',
        fgHex: '#f5f5f5',
        mutedHex: '#a8a8a8',
        trackHex: '#2c2c2c',
        heroTrackHex: '#262626',
        okFillHex: '#bdbdbd',
        warnFillHex: '#f0b44c',
        critFillHex: '#ff4d4d',
    }),
    makeThemePreset({
        id: 'nord',
        label: 'Nord',
        category: 'dark',
        isLight: false,
        swatchHex: '#2e3440',
        accentHex: '#88c0d0',
        bgHex: '#2e3440',
        fgHex: '#f2f5fa',
        mutedHex: '#bac4d4',
        trackHex: '#464f62',
        heroTrackHex: '#414a5c',
        okFillHex: '#88c0d0',
        warnFillHex: '#ebcb8b',
        critFillHex: '#ff5c67',
    }),
    makeThemePreset({
        id: 'osaka-jade',
        label: 'Osaka Jade',
        category: 'dark',
        isLight: false,
        swatchHex: '#111c18',
        accentHex: '#54c59f',
        bgHex: '#111c18',
        fgHex: '#e6f5ee',
        mutedHex: '#9ec2b4',
        trackHex: '#243b32',
        heroTrackHex: '#1f342c',
        okFillHex: '#54c59f',
        warnFillHex: '#e5c065',
        critFillHex: '#ff595e',
    }),
    makeThemePreset({
        id: 'ristretto',
        label: 'Ristretto',
        category: 'dark',
        isLight: false,
        swatchHex: '#2c2525',
        accentHex: '#f38d70',
        bgHex: '#2c2525',
        fgHex: '#f7eeec',
        mutedHex: '#c6b5b3',
        trackHex: '#473c3c',
        heroTrackHex: '#423838',
        okFillHex: '#f38d70',
        warnFillHex: '#f9cc6c',
        critFillHex: '#fd6883',
    }),
    makeThemePreset({
        id: 'rose-pine',
        label: 'Rosé Pine',
        category: 'dark',
        isLight: false,
        swatchHex: '#1f1d2e',
        accentHex: '#c4a7e7',
        bgHex: '#1f1d2e',
        fgHex: '#ebe9fa',
        mutedHex: '#afaac7',
        trackHex: '#36334c',
        heroTrackHex: '#322f47',
        okFillHex: '#9ccfd8',
        warnFillHex: '#f6c177',
        critFillHex: '#eb6f92',
    }),
    makeThemePreset({
        id: 'ethereal',
        label: 'Ethereal',
        category: 'dark',
        isLight: false,
        swatchHex: '#0c1020',
        accentHex: '#7daeff',
        bgHex: '#0c1020',
        fgHex: '#edf3ff',
        mutedHex: '#9eb0d4',
        trackHex: '#202945',
        heroTrackHex: '#1b233b',
        okFillHex: '#7daeff',
        warnFillHex: '#ebb95e',
        critFillHex: '#ff546b',
    }),
    makeThemePreset({
        id: 'hackerman',
        label: 'Hackerman',
        category: 'dark',
        isLight: false,
        swatchHex: '#0b1110',
        accentHex: '#39ff88',
        bgHex: '#0b1110',
        fgHex: '#e6fff0',
        mutedHex: '#8fc7a6',
        trackHex: '#1c3328',
        heroTrackHex: '#182b22',
        okFillHex: '#39ff88',
        warnFillHex: '#edd053',
        critFillHex: '#ff4d55',
    }),
    makeThemePreset({
        id: 'miasma',
        label: 'Miasma',
        category: 'dark',
        isLight: false,
        swatchHex: '#222222',
        accentHex: '#b4be82',
        bgHex: '#222222',
        fgHex: '#f2ede1',
        mutedHex: '#b8b3a4',
        trackHex: '#3a3934',
        heroTrackHex: '#35342f',
        okFillHex: '#b4be82',
        warnFillHex: '#d9ae59',
        critFillHex: '#de5d5d',
    }),
    makeThemePreset({
        id: 'vantablack',
        label: 'Vantablack',
        category: 'dark',
        isLight: false,
        swatchHex: '#050507',
        accentHex: '#8ab4f8',
        bgHex: '#050507',
        fgHex: '#f8f9fc',
        mutedHex: '#9ea5b5',
        trackHex: '#20222b',
        heroTrackHex: '#1b1d25',
        okFillHex: '#8ab4f8',
        warnFillHex: '#ebb654',
        critFillHex: '#ff4d57',
    }),
    makeThemePreset({
        id: 'retro-82',
        label: 'Retro 82',
        category: 'dark',
        isLight: false,
        swatchHex: '#171128',
        accentHex: '#ff6ad5',
        bgHex: '#171128',
        fgHex: '#faedff',
        mutedHex: '#bdabdb',
        trackHex: '#332552',
        heroTrackHex: '#2d2048',
        okFillHex: '#38e8ff',
        warnFillHex: '#ffd15c',
        critFillHex: '#ff4d7e',
    }),
]);

export const SYSTEM_LIGHT_PALETTE = makeThemePreset({
    id: 'system-light',
    label: 'GNOME System (Light)',
    category: 'glass',
    isLight: true,
    swatchHex: '#f8f9fc',
    accentHex: '#1a5fb4',
    bgHex: '#f8f9fc',
    fgHex: '#12151c',
    mutedHex: '#434b59',
    trackHex: '#d2d8e3',
    heroTrackHex: '#c8d0de',
    okFillHex: '#1c71d8',
    warnFillHex: '#c46800',
    critFillHex: '#c92a2a',
});

export const THEME_BY_ID = Object.freeze(
    Object.fromEntries(THEME_PRESETS.map(t => [t.id, t]))
);

/**
 * Resolves a theme preset safely, supporting `'system-light'` when the host GNOME
 * surface is light and defaulting unknown IDs to `midnight-slate`.
 *
 * @param {string} themeId
 */
export function getThemePalette(themeId) {
    if (themeId === 'system-light')
        return SYSTEM_LIGHT_PALETTE;
    return THEME_BY_ID[themeId] ?? THEME_BY_ID['midnight-slate'];
}

/**
 * Resolves whether the popover should render in single-column `'list'` (`352px`)
 * or wide 2-column `'grid'` (`688px`) mode.
 *
 * @param {string} layoutSetting `'list' | 'auto-grid' | 'grid'`
 * @param {number} visibleCount
 * @param {number} [monitorWidth=1920]
 * @returns {'list'|'grid'}
 */
export function resolveEffectivePopoverLayout(layoutSetting, visibleCount, monitorWidth = 1920) {
    const count = Number(visibleCount) || 0;
    const width = Number(monitorWidth) || 1920;
    if (layoutSetting === 'grid')
        return count >= 2 ? 'grid' : 'list';
    if (layoutSetting === 'auto-grid')
        return count >= 6 && width >= 760 ? 'grid' : 'list';
    return 'list';
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
 * Formats a bucket's display title so cryptic short codes (`wk`, `5h`, `31d`) always
 * expand into clear human-readable labels (`Weekly`, `5-Hour`, `Monthly`), while
 * keeping rich labels (`Fable`, `Sonnet`, `Opus`, `Extra Usage`, `Gemini · Weekly`,
 * `Claude & GPT · Weekly`, `Cursor Models`, `Other Models`, `Monthly`) intact.
 *
 * @param {object|string|null|undefined} bucket
 * @returns {string}
 */
export function formatBucketDisplayLabel(bucket) {
    const rawInput = typeof bucket === 'string'
        ? bucket
        : (bucket?.label || bucket?.short_label || bucket?.id || '');
    const cleaned = normalizeBucketLabel(rawInput);
    if (!cleaned)
        return 'Usage';

    const lower = cleaned.toLowerCase();
    if (lower === 'wk' || lower === '7d' || lower === 'weekly' || lower === 'weekly (7d)')
        return 'Weekly';
    if (
        lower === '5h' ||
        lower === '5-hour' ||
        lower === '5 hour' ||
        lower === 'five hour' ||
        lower === 'session' ||
        lower === 'session (5h)'
    )
        return '5-Hour';
    if (
        lower === '24h' ||
        lower === '24-hour' ||
        lower === '24 hour' ||
        lower === '1d' ||
        lower === 'daily' ||
        lower === 'daily (24h)'
    )
        return 'Daily';
    if (
        lower === '31d' ||
        lower === '30d' ||
        lower === 'monthly' ||
        lower === 'monthly (31d)' ||
        lower === 'monthly (30d)'
    )
        return 'Monthly';
    if (lower === 'premium' || lower === 'premium requests')
        return 'Premium Requests';
    if (lower === 'extra usage' || lower === 'extra')
        return 'Extra Usage';

    return cleaned
        .replace(/·\s*5h\b/gi, '· 5-Hour')
        .replace(/·\s*24h\b/gi, '· Daily')
        .replace(/·\s*wk\b/gi, '· Weekly')
        .replace(/·\s*31d\b/gi, '· Monthly');
}

/**
 * Resolves a quota bucket's reset window length in seconds, preferring explicit
 * `bucket.window_seconds` from the Rust collector and falling back to well-known
 * bucket IDs / labels (`5h`, `24h`/`daily`, `wk`/`weekly`/`7d`, `31d`/`monthly`/`premium`).
 *
 * @param {object|null|undefined} bucket
 * @returns {number} Window duration in seconds, or `0` if unknown
 */
export function inferBucketWindowSeconds(bucket) {
    if (!bucket || typeof bucket !== 'object')
        return 0;

    if (
        typeof bucket.window_seconds === 'number' &&
        Number.isFinite(bucket.window_seconds) &&
        bucket.window_seconds > 0
    ) {
        return Math.round(bucket.window_seconds);
    }

    const candidates = [bucket.id, bucket.short_label, bucket.label]
        .filter(v => typeof v === 'string' && v.trim() !== '')
        .map(v => v.trim().toLowerCase());

    for (const raw of candidates) {
        if (/\b(5h|5-hour|5\s*hour|five\s*hour|session)\b/.test(raw))
            return 18000;
        if (/\b(24h|24-hour|24\s*hour|1d|daily)\b/.test(raw))
            return 86400;
        if (/\b(wk|7d|weekly)\b/.test(raw))
            return 604800;
        if (/\b(31d|30d|monthly|premium)\b/.test(raw))
            return 2678400;
    }

    return 0;
}

/**
 * Computes calm-technology burn-rate / pace projection for a quota bucket relative
 * to elapsed window time (`5% <= elapsedFraction < 99%` and `used_percent > 0`).
 *
 * @param {object|null|undefined} bucket
 * @param {number} [nowEpoch]
 * @returns {{
 *   state: 'under' | 'on_track' | 'elevated' | 'over',
 *   ratio: number,
 *   projectedPercent: number,
 *   elapsedPercent: number,
 *   severity: 'normal' | 'warning' | 'critical',
 *   text: string
 * } | null}
 */
export function computeBucketPace(bucket, nowEpoch = Math.floor(Date.now() / 1000)) {
    if (!bucket || typeof bucket !== 'object')
        return null;

    const usedPercent = clampPercent(bucket.used_percent ?? 0);
    if (usedPercent <= 0)
        return null;

    const resetEpoch = bucket.resets_at_epoch;
    if (typeof resetEpoch !== 'number' || !Number.isFinite(resetEpoch) || resetEpoch <= 0)
        return null;

    const windowSec = inferBucketWindowSeconds(bucket);
    if (windowSec <= 0)
        return null;

    const remainingSec = resetEpoch - nowEpoch;
    if (remainingSec <= 0 || remainingSec >= windowSec)
        return null;

    const elapsedFraction = 1 - (remainingSec / windowSec);
    if (elapsedFraction < 0.05 || elapsedFraction >= 0.99)
        return null;

    const paceRatio = (usedPercent / 100) / elapsedFraction;
    const ratio = Math.round(paceRatio * 100) / 100;
    const projectedPercent = Math.round(((usedPercent / 100) / elapsedFraction) * 100);
    const elapsedPercent = Math.round(elapsedFraction * 100);

    let state;
    let text;
    if (usedPercent >= 100 || paceRatio > 1.35) {
        state = 'over';
        const mult = paceRatio >= 10 ? String(Math.round(paceRatio)) : paceRatio.toFixed(1);
        text = `Pace: ${mult}× sustainable · hits 100% before reset`;
    } else if (paceRatio > 1.0) {
        state = 'elevated';
        text = `Pace: Elevated (${paceRatio.toFixed(1)}×) · hits 100% near reset`;
    } else if (paceRatio >= 0.85) {
        state = 'on_track';
        text = `Pace: On track · ~${Math.min(100, projectedPercent)}% by reset`;
    } else {
        state = 'under';
        text = `Pace: Comfortable · ${usedPercent}% used at ${elapsedPercent}% of window`;
    }

    let severity = 'normal';
    if (projectedPercent >= 95 && ratio > 1.15)
        severity = state === 'over' ? 'critical' : 'warning';

    return {
        state,
        ratio,
        projectedPercent,
        elapsedPercent,
        severity,
        text,
    };
}

/**
 * Normalizes raw 7-day sparkline points (`[{ts, used}, ...]`) into sorted `[0, 1]`
 * plot coordinates and summary statistics. Returns `null` when fewer than 2 valid
 * points are present.
 *
 * @param {Array<{ts: number, used: number}>|null|undefined} rawPoints
 * @param {'used'|'remaining'} [metricMode='used']
 * @returns {{
 *   points: Array<{ts: number, used: number, displayValue: number, xNorm: number, yNorm: number}>,
 *   minDisplay: number,
 *   maxDisplay: number,
 *   firstDisplay: number,
 *   latestDisplay: number,
 *   latestUsed: number,
 *   deltaDisplay: number,
 *   spanSeconds: number,
 *   severity: 'normal'|'warning'|'critical',
 *   summaryText: string
 * } | null}
 */
export function normalizeSparklinePoints(rawPoints, metricMode = 'used') {
    if (!Array.isArray(rawPoints) || rawPoints.length < 2)
        return null;

    const byTs = new Map();
    for (const item of rawPoints) {
        if (!item || typeof item !== 'object')
            continue;
        const ts = Number(item.ts);
        const usedRaw = Number(item.used);
        if (!Number.isFinite(ts) || ts <= 0 || !Number.isFinite(usedRaw))
            continue;
        byTs.set(Math.floor(ts), clampPercent(usedRaw));
    }

    if (byTs.size < 2)
        return null;

    const sortedEntries = [...byTs.entries()].sort((a, b) => a[0] - b[0]);
    const minTs = sortedEntries[0][0];
    const maxTs = sortedEntries[sortedEntries.length - 1][0];
    const span = Math.max(1, maxTs - minTs);
    const isRemaining = metricMode === 'remaining';

    let minDisplay = 100;
    let maxDisplay = 0;

    const points = sortedEntries.map(([ts, used]) => {
        const displayValue = isRemaining ? Math.max(0, 100 - used) : used;
        minDisplay = Math.min(minDisplay, displayValue);
        maxDisplay = Math.max(maxDisplay, displayValue);
        return {
            ts,
            used,
            displayValue,
            xNorm: (ts - minTs) / span,
            yNorm: 1 - (displayValue / 100),
        };
    });

    const firstDisplay = points[0].displayValue;
    const latest = points[points.length - 1];
    const deltaDisplay = latest.displayValue - firstDisplay;
    const sign = deltaDisplay > 0 ? `+${deltaDisplay}%` : `${deltaDisplay}%`;
    const spanHours = Math.max(1, Math.round(span / 3600));
    const spanLabel = spanHours >= 24 ? `${Math.round(spanHours / 24)}d` : `${spanHours}h`;
    const modeWord = isRemaining ? 'left' : 'used';

    return {
        points,
        minDisplay,
        maxDisplay,
        firstDisplay,
        latestDisplay: latest.displayValue,
        latestUsed: latest.used,
        deltaDisplay,
        spanSeconds: span,
        severity: getSeverityForUsedPercent(latest.used),
        summaryText: `${latest.displayValue}% ${modeWord} (${sign} / ${spanLabel})`,
    };
}

/**
 * Formats ordered buckets for the Detailed main popover card so every bucket
 * renders on its own dedicated full-width line with zero side-by-side truncation.
 *
 * @param {Array<object>} buckets
 * @returns {Array<{kind: 'full', item: object}>}
 */
export function groupBucketsForDetailedView(buckets) {
    if (!Array.isArray(buckets) || buckets.length === 0)
        return [];

    const groups = [];
    for (const bucket of buckets) {
        if (!bucket)
            continue;
        groups.push({kind: 'full', item: bucket});
    }
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
 * Formats a concise, non-redundant extra-info / credits note for a provider card
 * when explicit `extra_info` is present, active on-demand spend (`used_dollars > 0`) exists,
 * or a prepaid balance is available on an account without a plan tier badge.
 *
 * @param {object|null|undefined} account
 * @param {boolean} [hasPlanBadge=false]
 * @returns {string}
 */
export function formatCompactAccountExtraNote(account, hasPlanBadge = false) {
    if (!account || typeof account !== 'object')
        return '';

    if (typeof account.extra_info === 'string' && account.extra_info.trim() !== '')
        return account.extra_info.trim();

    const credits = account.credits;
    if (!credits || typeof credits !== 'object')
        return '';

    const label = credits.label || 'Credits';
    const hasUsed = typeof credits.used_dollars === 'number' && Number.isFinite(credits.used_dollars);
    const hasLimit = typeof credits.limit_dollars === 'number' && Number.isFinite(credits.limit_dollars);
    const hasBalance = typeof credits.balance_dollars === 'number' && Number.isFinite(credits.balance_dollars);

    if (hasUsed && credits.used_dollars > 0) {
        if (hasLimit && credits.limit_dollars > 0)
            return `${label}: $${credits.used_dollars.toFixed(2)} / $${credits.limit_dollars.toFixed(2)}`;
        return `${label}: $${credits.used_dollars.toFixed(2)} used`;
    }
    if (!hasPlanBadge && hasBalance && credits.balance_dollars > 0)
        return `${label}: $${credits.balance_dollars.toFixed(2)}`;
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
 * Computes alert severity (`'normal' | 'warning' | 'critical'`) for a provider card,
 * factoring in primary usage, bucket usage, auth sign-out, and collection errors.
 *
 * @param {object|null|undefined} provider
 * @returns {'normal'|'warning'|'critical'}
 */
export function getProviderAlertSeverity(provider) {
    if (!provider || typeof provider !== 'object')
        return 'normal';

    const activeAcc = getActiveAccount(provider);
    let worstUsed = typeof provider.primary_used_percent === 'number'
        ? clampPercent(provider.primary_used_percent)
        : 0;

    if (Array.isArray(activeAcc?.buckets)) {
        for (const b of activeAcc.buckets) {
            if (typeof b?.used_percent === 'number')
                worstUsed = Math.max(worstUsed, clampPercent(b.used_percent));
        }
    }

    let severity = getSeverityForUsedPercent(worstUsed);
    if (activeAcc?.state === 'error' || activeAcc?.error) {
        if (severity === 'normal')
            severity = 'warning';
    }
    if (activeAcc?.state === 'signed_out') {
        if (severity === 'normal')
            severity = 'warning';
    }
    return severity;
}

/**
 * Summarizes overflow providers (`6+` in compact list mode) for progressive disclosure:
 * computes badge severity and renders `Show N more agents (OpenRouter · Copilot…) ▾`
 * or `Show fewer ▴`.
 *
 * @param {Array<object>} overflowProviders
 * @param {boolean} [expanded=false]
 * @returns {{
 *   count: number,
 *   alertSeverity: 'normal'|'warning'|'critical',
 *   hasAlert: boolean,
 *   label: string
 * }}
 */
export function summarizeOverflowProviders(overflowProviders, expanded = false) {
    const items = Array.isArray(overflowProviders) ? overflowProviders.filter(Boolean) : [];
    const count = items.length;
    if (count === 0) {
        return {
            count: 0,
            alertSeverity: 'normal',
            hasAlert: false,
            label: '',
        };
    }

    let alertSeverity = 'normal';
    for (const prov of items) {
        const sev = getProviderAlertSeverity(prov);
        if (sev === 'critical') {
            alertSeverity = 'critical';
            break;
        }
        if (sev === 'warning')
            alertSeverity = 'warning';
    }

    if (expanded) {
        return {
            count,
            alertSeverity,
            hasAlert: alertSeverity !== 'normal',
            label: 'Show fewer ▴',
        };
    }

    const previewNames = items.slice(0, 2).map(p => {
        const rawName = String(p.display_name || PROVIDER_BY_ID[p.id]?.displayName || p.id || 'Agent');
        return rawName
            .replace(/^GitHub\s+/i, '')
            .replace(/\s+CLI$/i, '')
            .trim();
    });
    const previewStr = previewNames.join(' · ') + (count > 2 ? '…' : '');
    const noun = count === 1 ? 'agent' : 'agents';

    return {
        count,
        alertSeverity,
        hasAlert: alertSeverity !== 'normal',
        label: `Show ${count} more ${noun} (${previewStr}) ▾`,
    };
}

/**
 * Formats a clean plain-text usage summary suitable for 1-click clipboard copying.
 *
 * @param {Array<object>} visibleProviders
 * @param {'used'|'remaining'} [metricMode='used']
 * @param {number} [nowEpoch]
 * @returns {string}
 */
export function formatClipboardSummary(
    visibleProviders,
    metricMode = 'used',
    nowEpoch = Math.floor(Date.now() / 1000)
) {
    const providers = Array.isArray(visibleProviders) ? visibleProviders.filter(Boolean) : [];
    if (providers.length === 0)
        return 'AI Agent Usage: No active AI agents detected';

    const isRemaining = metricMode === 'remaining';
    const modeSuffix = isRemaining ? 'left' : 'used';
    const lines = [`AI Agent Usage (${isRemaining ? '% remaining' : '% used'})`];

    for (const prov of providers) {
        const name = prov.display_name || PROVIDER_BY_ID[prov.id]?.displayName || prov.id || 'Agent';
        const activeAcc = getActiveAccount(prov);
        const tier = prov.plan_badge || activeAcc?.plan_tier || '';
        const head = tier ? `${name} (${tier})` : name;

        if (activeAcc?.state === 'signed_out') {
            lines.push(`- ${head}: Signed out`);
            continue;
        }

        const buckets = Array.isArray(activeAcc?.buckets) ? activeAcc.buckets : [];
        if (buckets.length > 0) {
            const parts = buckets.map(b => {
                const m = formatBucketMetric(b, metricMode);
                const title = formatBucketDisplayLabel(b);
                const reset = formatResetCountdown(b.resets_at_epoch, nowEpoch, {prefix: false});
                const resetSuffix = reset && reset !== 'now' ? ` (resets in ${reset})` : '';
                return `${title} ${m.percentText} ${modeSuffix}${resetSuffix}`;
            });
            const creditsNote = formatCompactAccountExtraNote(activeAcc, Boolean(tier));
            if (creditsNote)
                parts.push(creditsNote);
            lines.push(`- ${head}: ${parts.join(' · ')}`);
            continue;
        }

        if (typeof prov.primary_used_percent === 'number') {
            const m = formatBucketMetric({
                used_percent: prov.primary_used_percent,
                remaining_percent: prov.primary_remaining_percent,
            }, metricMode);
            const reset = formatResetCountdown(prov.primary_reset_epoch, nowEpoch, {prefix: false});
            const resetSuffix = reset && reset !== 'now' ? ` (resets in ${reset})` : '';
            lines.push(`- ${head}: ${m.percentText} ${modeSuffix}${resetSuffix}`);
            continue;
        }

        const fallback = formatCreditsSummary(activeAcc?.credits) ||
            activeAcc?.extra_info ||
            activeAcc?.error ||
            activeAcc?.status_note ||
            'Idle';
        lines.push(`- ${head}: ${fallback}`);
    }

    return lines.join('\n');
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
                dashboard_url: meta?.dashboardUrl || null,
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
                        sparkline: [],
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
                    label: formatBucketDisplayLabel(b),
                    short_label: b?.short_label || 'wk',
                    layout_hint: 'full_row',
                    used_percent: used,
                    remaining_percent: rem,
                };
            });

            const sparkline = Array.isArray(acc?.sparkline)
                ? acc.sparkline
                    .filter(pt => pt && Number.isFinite(Number(pt.ts)) && Number.isFinite(Number(pt.used)))
                    .map(pt => ({
                        ts: Math.floor(Number(pt.ts)),
                        used: clampPercent(Number(pt.used)),
                    }))
                : [];

            return {
                ...acc,
                buckets,
                sparkline,
                login_command: acc?.login_command || meta?.defaultLoginCommand || null,
            };
        });

        const normalizedProvider = {
            ...prov,
            display_name: prov?.display_name || meta?.displayName || prov?.id || 'Agent',
            icon_name: prov?.icon_name || meta?.iconName || 'agent-status-symbolic',
            dashboard_url: prov?.dashboard_url || meta?.dashboardUrl || null,
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
 * Orders providers according to the user's `provider-order` list, appending any
 * valid canonical provider omitted from `orderIds` in canonical `DEFAULT_PROVIDER_ORDER`
 * sequence (and preserving any unknown custom provider IDs afterward) so newly
 * added providers are never lost.
 *
 * @param {Array<object>} providers
 * @param {ReadonlyArray<string>} [orderIds=DEFAULT_PROVIDER_ORDER]
 * @returns {Array<object>}
 */
export function sortProvidersByOrder(providers, orderIds = DEFAULT_PROVIDER_ORDER) {
    if (!Array.isArray(providers) || providers.length === 0)
        return [];

    const rankById = new Map();
    let nextRank = 0;

    if (Array.isArray(orderIds)) {
        for (const rawId of orderIds) {
            if (typeof rawId !== 'string')
                continue;
            const id = rawId.trim();
            if (id && !rankById.has(id))
                rankById.set(id, nextRank++);
        }
    }

    for (const canonicalId of DEFAULT_PROVIDER_ORDER) {
        if (!rankById.has(canonicalId))
            rankById.set(canonicalId, nextRank++);
    }

    return providers
        .map((prov, originalIndex) => ({prov, originalIndex}))
        .filter(({prov}) => Boolean(prov && typeof prov === 'object'))
        .sort((a, b) => {
            const rankA = rankById.has(a.prov.id)
                ? rankById.get(a.prov.id)
                : (1000 + a.originalIndex);
            const rankB = rankById.has(b.prov.id)
                ? rankById.get(b.prov.id)
                : (1000 + b.originalIndex);
            if (rankA !== rankB)
                return rankA - rankB;
            return a.originalIndex - b.originalIndex;
        })
        .map(({prov}) => prov);
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
 * Evaluates smart quota warning, critical, and reset notification transitions across
 * providers while mutating `bucketStates` in-place.
 *
 * @param {Map<string, object>} bucketStates
 * @param {object} report
 * @param {{
 *   isInitialHydration?: boolean,
 *   nowEpoch?: number,
 *   hiddenProviderIds?: ReadonlyArray<string>,
 *   providerOrder?: ReadonlyArray<string>,
 *   notifyOnWarning?: boolean,
 *   notifyOnCritical?: boolean,
 *   notifyOnReset?: boolean
 * }} [options]
 * @returns {Array<{
 *   kind: 'warning' | 'critical' | 'reset',
 *   providerId: string,
 *   title: string,
 *   body: string,
 *   urgency: 'normal' | 'critical'
 * }>}
 */
export function evaluateQuotaNotificationStep(
    bucketStates,
    report,
    {
        isInitialHydration = false,
        nowEpoch = Math.floor(Date.now() / 1000),
        hiddenProviderIds = [],
        providerOrder = DEFAULT_PROVIDER_ORDER,
        notifyOnWarning = false,
        notifyOnCritical = true,
        notifyOnReset = true,
    } = {}
) {
    if (!(bucketStates instanceof Map) || !report || typeof report !== 'object')
        return [];

    const normalized = normalizeStatusReport(report, nowEpoch);
    const allProviders = sortProvidersByOrder(normalized.providers ?? [], providerOrder);
    const visibleSet = new Set(
        filterVisibleProviders(allProviders, hiddenProviderIds).map(p => p.id)
    );

    const emitted = [];

    for (const prov of allProviders) {
        const accounts = Array.isArray(prov?.accounts) ? prov.accounts : [];
        const activeAcc = getActiveAccount(prov);

        for (const acc of accounts) {
            if (!acc || acc.state === 'signed_out')
                continue;

            const primaryBucket =
                acc.buckets?.find(b => b.is_primary) ?? acc.buckets?.[0] ?? null;
            if (!primaryBucket)
                continue;

            const bucketId = primaryBucket.id || primaryBucket.short_label || 'primary';
            const key = `${prov.id}::${acc.id || 'default'}::${bucketId}`;
            const usedPercent = clampPercent(primaryBucket.used_percent ?? 0);
            const resetsAtEpoch =
                typeof primaryBucket.resets_at_epoch === 'number' &&
                Number.isFinite(primaryBucket.resets_at_epoch) &&
                primaryBucket.resets_at_epoch > 0
                    ? primaryBucket.resets_at_epoch
                    : null;
            const windowToken = resetsAtEpoch !== null ? String(resetsAtEpoch) : 'static';

            const isTargetActive = activeAcc === acc && visibleSet.has(prov.id);
            const prev = bucketStates.get(key);

            if (isInitialHydration || !prev) {
                bucketStates.set(key, {
                    usedPercent,
                    resetsAtEpoch,
                    notifiedWarningWindow: usedPercent >= 80 ? windowToken : null,
                    notifiedCriticalWindow: usedPercent >= 90 ? windowToken : null,
                });
                continue;
            }

            let notifiedWarningWindow = prev.notifiedWarningWindow ?? null;
            let notifiedCriticalWindow = prev.notifiedCriticalWindow ?? null;

            const hasPrevReset =
                typeof prev.resetsAtEpoch === 'number' && prev.resetsAtEpoch > 0;
            const hasCurrReset = resetsAtEpoch !== null;
            const resetByClock = hasPrevReset && nowEpoch >= (prev.resetsAtEpoch - 60);
            const resetByNewWindow =
                hasPrevReset &&
                hasCurrReset &&
                resetsAtEpoch > (prev.resetsAtEpoch + 60);

            if (hasPrevReset && hasCurrReset && resetsAtEpoch > (prev.resetsAtEpoch + 60)) {
                notifiedWarningWindow = null;
                notifiedCriticalWindow = null;
            }

            const bucketLabel = formatBucketDisplayLabel(primaryBucket);

            if (prev.usedPercent >= 80 && usedPercent < 20 && (resetByClock || resetByNewWindow)) {
                notifiedWarningWindow = null;
                notifiedCriticalWindow = null;
                if (isTargetActive && notifyOnReset) {
                    emitted.push({
                        kind: 'reset',
                        providerId: prov.id,
                        title: `${prov.display_name} Quota Reset`,
                        body: `${bucketLabel} quota is fresh again (${usedPercent}% used)`,
                        urgency: 'normal',
                    });
                }
            } else if (prev.usedPercent < 90 && usedPercent >= 90) {
                if (isTargetActive && notifiedCriticalWindow !== windowToken) {
                    notifiedCriticalWindow = windowToken;
                    notifiedWarningWindow = windowToken;
                    if (notifyOnCritical) {
                        const countdown = formatResetCountdown(resetsAtEpoch, nowEpoch, {
                            prefix: false,
                        });
                        const suffix =
                            countdown && countdown !== 'now' ? ` (resets in ${countdown})` : '';
                        emitted.push({
                            kind: 'critical',
                            providerId: prov.id,
                            title: `${prov.display_name} Quota Critical (${usedPercent}%)`,
                            body: `${bucketLabel} usage reached ${usedPercent}%${suffix}`,
                            urgency: 'critical',
                        });
                    }
                }
            } else if (prev.usedPercent < 80 && usedPercent >= 80 && usedPercent < 90) {
                if (isTargetActive && notifiedWarningWindow !== windowToken) {
                    notifiedWarningWindow = windowToken;
                    if (notifyOnWarning) {
                        const countdown = formatResetCountdown(resetsAtEpoch, nowEpoch, {
                            prefix: false,
                        });
                        const suffix =
                            countdown && countdown !== 'now' ? ` (resets in ${countdown})` : '';
                        emitted.push({
                            kind: 'warning',
                            providerId: prov.id,
                            title: `${prov.display_name} Quota Warning`,
                            body: `${bucketLabel} usage reached ${usedPercent}%${suffix}`,
                            urgency: 'normal',
                        });
                    }
                }
            }

            bucketStates.set(key, {
                usedPercent,
                resetsAtEpoch,
                notifiedWarningWindow,
                notifiedCriticalWindow,
            });
        }
    }

    return emitted;
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
 * Returns the canonical deduplicated list of credential file/directory paths watched via
 * `Gio.FileMonitor` (filesystem events only — never read by the extension) so
 * completing `[Sign in]` or configuring API keys triggers a live refresh.
 *
 * @param {string} homeDir
 * @param {string|null} [configDir=null]
 * @returns {string[]}
 */
export function defaultCredentialWatchPaths(homeDir, configDir = null) {
    const cleanHome = String(homeDir || '').replace(/\/+$/, '');
    if (!cleanHome)
        return [];

    const defaultConfig = `${cleanHome}/.config`;
    const cleanConfig = configDir
        ? String(configDir).replace(/\/+$/, '')
        : defaultConfig;

    const candidates = [
        `${cleanHome}/.claude/.credentials.json`,
        `${cleanHome}/.codex/auth.json`,
        `${cleanHome}/.grok/auth.json`,
        `${defaultConfig}/gh/hosts.yml`,
        `${defaultConfig}/github-copilot/hosts.json`,
        `${cleanHome}/.gemini/oauth_creds.json`,
        `${defaultConfig}/openrouter`,
        `${defaultConfig}/Windsurf/User/globalStorage/state.vscdb`,
        `${defaultConfig}/deepseek`,
    ];

    if (cleanConfig && cleanConfig !== defaultConfig) {
        candidates.push(
            `${cleanConfig}/gh/hosts.yml`,
            `${cleanConfig}/github-copilot/hosts.json`,
            `${cleanConfig}/openrouter`,
            `${cleanConfig}/Windsurf/User/globalStorage/state.vscdb`,
            `${cleanConfig}/deepseek`
        );
    }

    return [...new Set(candidates)];
}

/**
 * Quotes an arbitrary string as a single literal POSIX `sh` token (`'...'`),
 * stripping NUL bytes and escaping embedded single quotes as `'\''`.
 *
 * @param {string} value
 * @returns {string}
 */
export function posixShellQuote(value) {
    const clean = String(value ?? '').replace(/\0/g, '');
    return `'${clean.replace(/'/g, "'\\''")}'`;
}

/**
 * Tokenizes a login command string into literal argv tokens using POSIX-style
 * single/double quote grouping without performing any shell expansion (`$`,
 * backticks, `;`, `|`, `&`, redirects, and newlines are never interpreted as
 * shell operators).
 *
 * @param {string} raw
 * @returns {string[]}
 */
export function tokenizeLoginCommand(raw) {
    const text = String(raw ?? '').replace(/\0/g, '');
    const tokens = [];
    let current = '';
    let hasToken = false;
    let inSingle = false;
    let inDouble = false;

    for (let i = 0; i < text.length; i++) {
        const ch = text[i];

        if (inSingle) {
            if (ch === '\'')
                inSingle = false;
            else
                current += ch;
            continue;
        }

        if (inDouble) {
            if (ch === '"') {
                inDouble = false;
            } else if (ch === '\\' && i + 1 < text.length && (text[i + 1] === '"' || text[i + 1] === '\\')) {
                current += text[i + 1];
                i++;
            } else {
                current += ch;
            }
            continue;
        }

        if (/\s/.test(ch)) {
            if (hasToken) {
                tokens.push(current);
                current = '';
                hasToken = false;
            }
            continue;
        }

        hasToken = true;
        if (ch === '\'') {
            inSingle = true;
        } else if (ch === '"') {
            inDouble = true;
        } else if (ch === '\\' && i + 1 < text.length) {
            current += text[i + 1];
            i++;
        } else {
            current += ch;
        }
    }

    if (hasToken)
        tokens.push(current);

    return tokens;
}

/**
 * Quotes a command or argument token safely using `quoteFn`, expanding a leading
 * `~` or `~/` prefix without allowing shell metacharacter injection.
 *
 * @param {string} token
 * @param {(s: string) => string} quoteFn
 * @param {string|null} homeDir
 * @param {boolean} expandTilde
 * @returns {string}
 */
function quoteShellTokenSafe(token, quoteFn, homeDir, expandTilde) {
    const clean = String(token ?? '').replace(/\0/g, '');
    if (expandTilde && (clean === '~' || clean.startsWith('~/'))) {
        const rest = clean.slice(1);
        if (homeDir) {
            const trimmedHome = String(homeDir).replace(/\/+$/, '');
            return quoteFn(`${trimmedHome}${rest}`);
        }
        return rest === '' ? '"$HOME"' : `"$HOME"${quoteFn(rest)}`;
    }
    return quoteFn(clean);
}

/**
 * Quotes a `KEY=VALUE` environment assignment safely using `quoteFn`, expanding
 * a leading `~` or `~/` in `VALUE` without allowing shell metacharacter injection.
 *
 * @param {string} assignment
 * @param {(s: string) => string} quoteFn
 * @param {string|null} homeDir
 * @returns {string}
 */
function quoteEnvAssignmentSafe(assignment, quoteFn, homeDir) {
    const clean = String(assignment ?? '').replace(/\0/g, '');
    const eqIdx = clean.indexOf('=');
    if (eqIdx <= 0)
        return quoteFn(clean);

    const key = clean.slice(0, eqIdx);
    const val = clean.slice(eqIdx + 1);
    if (val === '~' || val.startsWith('~/')) {
        const rest = val.slice(1);
        if (homeDir) {
            const trimmedHome = String(homeDir).replace(/\/+$/, '');
            return quoteFn(`${key}=${trimmedHome}${rest}`);
        }
        return rest === ''
            ? `${quoteFn(`${key}=`)}"$HOME"`
            : `${quoteFn(`${key}=`)}"$HOME"${quoteFn(rest)}`;
    }
    return quoteFn(clean);
}

/**
 * Builds terminal invocation argv for launching an interactive `login_command`
 * across standard Linux terminal emulators, quoting every custom token via
 * `quoteFn` (`GLib.shell_quote` in GNOME Shell or `posixShellQuote` in pure JS)
 * so spaces and shell metacharacters can never cause command injection.
 *
 * @param {string} terminalBinary
 * @param {string} loginCommand
 * @param {{
 *   quoteFn?: (value: string) => string,
 *   homeDir?: string|null,
 *   pretokenizedArgv?: string[]|null
 * }} [options]
 * @returns {string[]}
 */
export function buildTerminalArgv(terminalBinary, loginCommand, options = {}) {
    const quoteFn = typeof options?.quoteFn === 'function' ? options.quoteFn : posixShellQuote;
    const homeDir = options?.homeDir ? String(options.homeDir) : null;
    const pretokenized = Array.isArray(options?.pretokenizedArgv) && options.pretokenizedArgv.length > 0
        ? options.pretokenizedArgv
        : null;

    const baseName = String(terminalBinary || '').split('/').pop();
    const rawCmd = String(loginCommand || '').replace(/\0/g, '').trim();

    let safeCmd;
    if (pretokenized) {
        const quotedArgs = pretokenized.map((tok, idx) =>
            quoteShellTokenSafe(tok, quoteFn, homeDir, idx === 0)
        );
        safeCmd = String(pretokenized[0] ?? '').startsWith('-')
            ? `env -- ${quotedArgs.join(' ')}`
            : quotedArgs.join(' ');
    } else if (rawCmd === '') {
        safeCmd = 'echo "No login command configured"';
    } else if (rawCmd === 'agy auth login') {
        safeCmd = 'if command -v agy >/dev/null 2>&1; then agy auth login; elif command -v antigravity >/dev/null 2>&1; then antigravity; else agy auth login; fi';
    } else {
        const tokens = tokenizeLoginCommand(rawCmd);
        if (tokens.length === 0) {
            safeCmd = 'echo "No login command configured"';
        } else {
            const envTokens = [];
            let idx = 0;
            while (
                idx < tokens.length - 1 &&
                /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[idx])
            ) {
                envTokens.push(tokens[idx]);
                idx++;
            }
            const cmdTokens = tokens.slice(idx);
            const quotedCmd = cmdTokens.map((tok, cIdx) =>
                quoteShellTokenSafe(tok, quoteFn, homeDir, cIdx === 0)
            );

            if (envTokens.length > 0) {
                const quotedEnv = envTokens.map(tok =>
                    quoteEnvAssignmentSafe(tok, quoteFn, homeDir)
                );
                safeCmd = `env ${quotedEnv.join(' ')} ${quotedCmd.join(' ')}`;
            } else if (String(cmdTokens[0] ?? '').startsWith('-')) {
                safeCmd = `env -- ${quotedCmd.join(' ')}`;
            } else {
                safeCmd = quotedCmd.join(' ');
            }
        }
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
