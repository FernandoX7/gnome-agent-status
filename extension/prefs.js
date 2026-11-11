import Adw from 'gi://Adw';
import Gdk from 'gi://Gdk';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {
    CANONICAL_PROVIDERS,
    DEFAULT_PROVIDER_ORDER,
    DENSITY_MODES,
    FONT_STYLES,
    METRIC_MODES,
    PANEL_BOXES,
    PANEL_MODES,
    PROVIDER_BY_ID,
    THEME_PRESETS,
    buildConfigUpsertArgs,
    slugifyAccountId,
} from './format.js';

const BINARY_NAME = 'gnome-agent-status';
const TIMEOUT_SPIN_DEBOUNCE_MS = 250;
const PRESET_TOGGLE_SHORTCUT = '<Super><Shift>a';

export const POPOVER_LAYOUTS = Object.freeze([
    Object.freeze({id: 'list', label: 'Compact Single Column (Top 5 + More)'}),
    Object.freeze({id: 'auto-grid', label: 'Auto 2-Column Wide (6+ Agents)'}),
    Object.freeze({id: 'grid', label: 'Always 2-Column Wide'}),
]);

const DEFAULT_TOP5_VISIBLE_IDS = Object.freeze([
    'claude',
    'codex',
    'grok',
    'cursor',
    'agy',
]);

const FALLBACK_PROVIDER_SPECS = Object.freeze([
    Object.freeze({
        id: 'claude',
        displayName: 'Claude Code',
        locationHint: '~/.claude-work',
    }),
    Object.freeze({
        id: 'codex',
        displayName: 'OpenAI Codex',
        locationHint: '~/.codex-work',
    }),
    Object.freeze({
        id: 'grok',
        displayName: 'xAI Grok',
        locationHint: '~/.grok-work',
    }),
    Object.freeze({
        id: 'cursor',
        displayName: 'Cursor',
        locationHint: '~/.config/Cursor/User/globalStorage/state.vscdb',
    }),
    Object.freeze({
        id: 'agy',
        displayName: 'Google Antigravity',
        locationHint: '~/.gemini/antigravity',
    }),
    Object.freeze({
        id: 'openrouter',
        displayName: 'OpenRouter',
        locationHint: '~/.config/openrouter/api_key',
    }),
    Object.freeze({
        id: 'copilot',
        displayName: 'GitHub Copilot',
        locationHint: '~/.config/github-copilot/hosts.json',
    }),
    Object.freeze({
        id: 'gemini',
        displayName: 'Gemini CLI',
        locationHint: '~/.gemini/oauth_creds.json',
    }),
    Object.freeze({
        id: 'windsurf',
        displayName: 'Windsurf',
        locationHint: '~/.config/Windsurf/User/globalStorage/state.vscdb',
    }),
    Object.freeze({
        id: 'deepseek',
        displayName: 'DeepSeek',
        locationHint: '~/.config/deepseek/api_key',
    }),
]);

const FALLBACK_SPEC_BY_ID = Object.freeze(
    Object.fromEntries(FALLBACK_PROVIDER_SPECS.map(s => [s.id, s]))
);

/**
 * Resolves the canonical default provider priority order dynamically from `format.js`
 * (`DEFAULT_PROVIDER_ORDER` + `CANONICAL_PROVIDERS`), falling back cleanly to the 10-provider
 * schema priority order if `format.js` has not yet been updated during rebase.
 *
 * @returns {string[]}
 */
export function resolveDefaultProviderOrder() {
    const baseIds =
        Array.isArray(DEFAULT_PROVIDER_ORDER) &&
        DEFAULT_PROVIDER_ORDER.length >= FALLBACK_PROVIDER_SPECS.length
            ? DEFAULT_PROVIDER_ORDER
            : FALLBACK_PROVIDER_SPECS.map(s => s.id);

    const ordered = [];
    const seen = new Set();

    for (const rawId of baseIds) {
        const id = String(rawId ?? '').trim();
        if (id && !seen.has(id)) {
            seen.add(id);
            ordered.push(id);
        }
    }

    for (const extra of CANONICAL_PROVIDERS ?? []) {
        if (extra?.id && !seen.has(extra.id)) {
            seen.add(extra.id);
            ordered.push(extra.id);
        }
    }

    for (const spec of FALLBACK_PROVIDER_SPECS) {
        if (!seen.has(spec.id)) {
            seen.add(spec.id);
            ordered.push(spec.id);
        }
    }

    return ordered;
}

export const PREFS_PROVIDERS = Object.freeze((() => {
    const canonicalOrder = resolveDefaultProviderOrder();
    return canonicalOrder.map(id => {
        const fromFormat = PROVIDER_BY_ID?.[id] ??
            (CANONICAL_PROVIDERS ?? []).find(p => p?.id === id) ??
            null;
        const fallback = FALLBACK_SPEC_BY_ID[id] ?? null;
        return Object.freeze({
            ...(fromFormat ?? {}),
            id,
            displayName:
                fallback?.displayName ||
                fromFormat?.displayName ||
                id,
            locationHint:
                fromFormat?.locationHint ||
                fallback?.locationHint ||
                '',
        });
    });
})());

export const PREFS_PROVIDER_BY_ID = Object.freeze(
    Object.fromEntries(PREFS_PROVIDERS.map(p => [p.id, p]))
);

export const DEFAULT_HIDDEN_PROVIDERS = Object.freeze(
    PREFS_PROVIDERS
        .map(p => p.id)
        .filter(id => !DEFAULT_TOP5_VISIBLE_IDS.includes(id))
);

const POLL_MODE_OPTIONS = Object.freeze([
    Object.freeze({id: 'active', label: 'Active Polling'}),
    Object.freeze({id: 'passive', label: 'Passive Cache Only'}),
]);

/**
 * Checks safely whether `settings` exposes `key` in its compiled GSettings schema.
 *
 * @param {Gio.Settings} settings
 * @param {string} key
 * @returns {boolean}
 */
function hasSettingKey(settings, key) {
    try {
        return Boolean(settings?.settings_schema?.has_key?.(key));
    } catch (_e) {
        return false;
    }
}

/**
 * Builds dynamically formatted `{id, label}` choices from `THEME_PRESETS`, grouping by
 * category (`glass`, `light`, `dark`) and adding clear Light / Glass / Dark indicators
 * whenever `preset.category` (or `preset.isLight`) is present.
 *
 * @param {ReadonlyArray<object>} [presets=THEME_PRESETS]
 * @param {(s: string) => string} [translate=s => s]
 * @returns {Array<{id: string, label: string}>}
 */
export function buildThemeComboOptions(presets = THEME_PRESETS, translate = s => String(s ?? '')) {
    const list = Array.isArray(presets) ? presets.filter(p => p && p.id) : [];
    const hasCategories = list.some(p => typeof p.category === 'string' && p.category.trim() !== '');

    const ordered = [];
    if (hasCategories) {
        const categoryRank = ['glass', 'light', 'dark'];
        const seen = new Set();
        for (const cat of categoryRank) {
            for (const preset of list) {
                if (String(preset.category || '').toLowerCase() === cat && !seen.has(preset.id)) {
                    seen.add(preset.id);
                    ordered.push(preset);
                }
            }
        }
        for (const preset of list) {
            if (!seen.has(preset.id)) {
                seen.add(preset.id);
                ordered.push(preset);
            }
        }
    } else {
        ordered.push(...list);
    }

    return ordered.map(preset => {
        const baseLabel = translate(preset.label || preset.id);
        const cat = typeof preset.category === 'string' ? preset.category.trim().toLowerCase() : '';
        let badge = '';
        if (cat === 'glass')
            badge = translate('Glass');
        else if (cat === 'light')
            badge = translate('Light');
        else if (cat === 'dark')
            badge = translate('Dark');
        else if (cat !== '')
            badge = translate(preset.category);
        else if (typeof preset.isLight === 'boolean')
            badge = preset.isLight ? translate('Light') : translate('Dark');

        return {
            id: preset.id,
            label: badge ? `${baseLabel}  ·  ${badge}` : baseLabel,
        };
    });
}

/**
 * Parses and normalizes the first valid GTK accelerator from a `keybinding-toggle-popover` array.
 *
 * @param {ReadonlyArray<string>|null|undefined} rawAccels
 * @returns {{accel: string, readable: string}}
 */
export function parseShortcutSetting(rawAccels) {
    if (!Array.isArray(rawAccels))
        return {accel: '', readable: ''};

    for (const entry of rawAccels) {
        const candidate = String(entry ?? '').trim();
        if (!candidate)
            continue;
        try {
            const [ok, keyval, mods] = Gtk.accelerator_parse(candidate);
            if (ok && keyval !== 0) {
                return {
                    accel: candidate,
                    readable: Gtk.accelerator_get_label(keyval, mods) || candidate,
                };
            }
        } catch (_e) {
            // Ignore malformed accelerator strings
        }
    }

    return {accel: '', readable: ''};
}

/**
 * Returns `true` when `keyval` is a bare modifier key rather than a trigger key.
 *
 * @param {number} keyval
 * @returns {boolean}
 */
function isStandaloneModifierKey(keyval) {
    return (
        keyval === Gdk.KEY_Shift_L ||
        keyval === Gdk.KEY_Shift_R ||
        keyval === Gdk.KEY_Control_L ||
        keyval === Gdk.KEY_Control_R ||
        keyval === Gdk.KEY_Alt_L ||
        keyval === Gdk.KEY_Alt_R ||
        keyval === Gdk.KEY_Meta_L ||
        keyval === Gdk.KEY_Meta_R ||
        keyval === Gdk.KEY_Super_L ||
        keyval === Gdk.KEY_Super_R ||
        keyval === Gdk.KEY_Hyper_L ||
        keyval === Gdk.KEY_Hyper_R ||
        keyval === Gdk.KEY_ISO_Level3_Shift ||
        keyval === Gdk.KEY_Caps_Lock ||
        keyval === Gdk.KEY_Num_Lock
    );
}

/**
 * Normalizes a raw `provider-order` string array so all canonical providers appear
 * exactly once, preserving custom user ordering and appending any newly added providers.
 *
 * @param {ReadonlyArray<string>|null|undefined} rawOrder
 * @returns {string[]}
 */
export function normalizeProviderOrder(rawOrder) {
    const result = [];
    const seen = new Set();

    if (Array.isArray(rawOrder)) {
        for (const item of rawOrder) {
            const id = String(item ?? '').trim();
            if (id && PREFS_PROVIDER_BY_ID[id] && !seen.has(id)) {
                seen.add(id);
                result.push(id);
            }
        }
    }

    for (const prov of PREFS_PROVIDERS) {
        if (!seen.has(prov.id)) {
            seen.add(prov.id);
            result.push(prov.id);
        }
    }

    return result;
}

/**
 * Returns a new normalized provider order with the item at `fromIndex` shifted by `delta`.
 *
 * @param {ReadonlyArray<string>} currentOrder
 * @param {number} fromIndex
 * @param {number} delta
 * @returns {string[]}
 */
export function moveProviderInOrder(currentOrder, fromIndex, delta) {
    const normalized = normalizeProviderOrder(currentOrder);
    const targetIndex = fromIndex + delta;
    if (
        fromIndex < 0 ||
        fromIndex >= normalized.length ||
        targetIndex < 0 ||
        targetIndex >= normalized.length ||
        fromIndex === targetIndex
    ) {
        return normalized;
    }

    const next = [...normalized];
    const [moved] = next.splice(fromIndex, 1);
    next.splice(targetIndex, 0, moved);
    return next;
}

/**
 * Escapes plain text for Libadwaita properties (`Adw.PreferencesGroup` title/description,
 * `Adw.PreferencesRow` title/subtitle, and `Adw.Toast` title) that parse Pango markup.
 *
 * @param {string|null|undefined} text
 * @returns {string}
 */
export function escapeMarkup(text) {
    return GLib.markup_escape_text(String(text ?? ''), -1);
}

/**
 * Returns `true` when an error from `Gtk.FileDialog` represents user dismissal or cancellation.
 *
 * @param {unknown} err
 * @returns {boolean}
 */
export function isDismissedDialogError(err) {
    if (!err || typeof err !== 'object')
        return false;
    return Boolean(
        err.matches?.(Gtk.DialogError, Gtk.DialogError.DISMISSED) ||
        err.matches?.(Gtk.DialogError, Gtk.DialogError.CANCELLED) ||
        err.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED)
    );
}

/**
 * Extracts a validated local filesystem path from a `Gio.File` chosen via `Gtk.FileDialog`.
 * Non-local GVFS URIs where `file.get_path()` returns `null` are rejected with a toast.
 *
 * @param {Gio.File|null|undefined} file
 * @param {(message: string) => void} [showToast]
 * @param {(s: string) => string} [translate]
 * @returns {string|null}
 */
export function extractLocalDialogPath(file, showToast, translate = s => String(s ?? '')) {
    if (!file)
        return null;
    const localPath = file.get_path();
    if (!localPath || localPath.trim() === '') {
        showToast?.(translate('Selected location must be a local filesystem path'));
        return null;
    }
    return localPath;
}

/**
 * Opens a GTK 4 `Gtk.FileDialog` (file or folder picker) tracked by `cli`'s cancellable set,
 * handling `Gtk.DialogError.DISMISSED` cleanly with zero unhandled promise rejections and
 * validating local filesystem paths via `file.get_path()`.
 *
 * @param {{
 *   window: Gtk.Window,
 *   cli: PrefsCliRunner,
 *   title: string,
 *   selectFolder?: boolean,
 *   showToast: (message: string) => void,
 *   translate?: (s: string) => string,
 *   onPathSelected: (localPath: string) => void
 * }} params
 */
function openLocalFileDialog({
    window,
    cli,
    title,
    selectFolder = false,
    showToast,
    translate = s => String(s ?? ''),
    onPathSelected,
}) {
    if (cli.isDestroyed)
        return;

    const dialog = new Gtk.FileDialog({
        title,
        modal: true,
    });
    const cancellable = cli.newCancellable();
    const onReady = (src, res) => {
        cli.releaseCancellable(cancellable);
        let chosenFile = null;
        try {
            chosenFile = selectFolder
                ? src.select_folder_finish(res)
                : src.open_finish(res);
        } catch (err) {
            if (cli.isDestroyed || isDismissedDialogError(err))
                return;
            showToast(translate('Could not select path: %s').replace('%s', err.message));
            return;
        }

        if (cli.isDestroyed)
            return;

        const localPath = extractLocalDialogPath(chosenFile, showToast, translate);
        if (localPath)
            onPathSelected(localPath);
    };

    if (selectFolder)
        dialog.select_folder(window, cancellable, onReady);
    else
        dialog.open(window, cancellable, onReady);
}

/**
 * Opens a modal GTK 4 key-capture dialog (`Gtk.EventControllerKey`) for `keybinding-toggle-popover`,
 * supporting live modifier+key capture, `[Use Super+Shift+A]` quick preset, `[Clear]`, and `Esc` to cancel.
 *
 * @param {{
 *   window: Gtk.Window,
 *   settings: Gio.Settings,
 *   cli: PrefsCliRunner,
 *   showToast: (message: string) => void,
 *   translate: (s: string) => string
 * }} params
 * @returns {Gtk.Window|null}
 */
export function openShortcutCaptureDialog({
    window,
    settings,
    cli,
    showToast,
    translate: _,
}) {
    if (cli.isDestroyed || !hasSettingKey(settings, 'keybinding-toggle-popover'))
        return null;

    const current = parseShortcutSetting(settings.get_strv('keybinding-toggle-popover'));

    const dialog = new Gtk.Window({
        title: _('Set Global Toggle Shortcut'),
        transient_for: window,
        modal: true,
        resizable: false,
        default_width: 440,
    });

    let closed = false;
    const closeDialog = () => {
        if (closed)
            return;
        closed = true;
        try {
            dialog.destroy();
        } catch (_e) {
            // Ignore late destroy errors
        }
    };
    cli.trackCleanup(closeDialog);
    dialog.connect('close-request', () => {
        closed = true;
        return false;
    });

    const contentBox = new Gtk.Box({
        orientation: Gtk.Orientation.VERTICAL,
        spacing: 16,
        margin_top: 20,
        margin_bottom: 20,
        margin_start: 24,
        margin_end: 24,
    });

    const headingLabel = new Gtk.Label({
        label: _('Press a keyboard shortcut to toggle Agent Usage Status'),
        wrap: true,
        xalign: 0.5,
        justify: Gtk.Justification.CENTER,
        css_classes: ['title-4'],
    });

    const hintLabel = new Gtk.Label({
        label: _(
            'Hold Super, Ctrl, or Alt with a key (for example Super+Shift+A), or press Esc to cancel.'
        ),
        wrap: true,
        xalign: 0.5,
        justify: Gtk.Justification.CENTER,
        css_classes: ['dim-label'],
    });

    const previewPill = new Gtk.ShortcutLabel({
        accelerator: current.accel,
        disabled_text: _('Waiting for shortcut…'),
        halign: Gtk.Align.CENTER,
        valign: Gtk.Align.CENTER,
    });

    const statusLabel = new Gtk.Label({
        label: current.readable
            ? _('Current shortcut: %s').replace('%s', current.readable)
            : _('Currently disabled'),
        wrap: true,
        xalign: 0.5,
        justify: Gtk.Justification.CENTER,
        css_classes: ['caption', 'dim-label'],
    });

    const actionsBox = new Gtk.Box({
        orientation: Gtk.Orientation.HORIZONTAL,
        spacing: 8,
        halign: Gtk.Align.END,
        margin_top: 8,
    });

    const cancelBtn = new Gtk.Button({
        label: _('Cancel'),
        valign: Gtk.Align.CENTER,
    });
    cancelBtn.connect('clicked', () => {
        closeDialog();
    });

    const clearBtn = new Gtk.Button({
        label: _('Clear'),
        tooltip_text: _('Disable global keyboard shortcut'),
        valign: Gtk.Align.CENTER,
        css_classes: ['destructive-action'],
    });
    clearBtn.connect('clicked', () => {
        if (!cli.isDestroyed) {
            settings.set_strv('keybinding-toggle-popover', []);
            showToast(_('Global toggle shortcut cleared'));
        }
        closeDialog();
    });

    const presetBtn = new Gtk.Button({
        label: _('Use Super+Shift+A'),
        tooltip_text: _('Bind global shortcut to Super+Shift+A'),
        valign: Gtk.Align.CENTER,
        css_classes: ['suggested-action'],
    });
    presetBtn.connect('clicked', () => {
        if (!cli.isDestroyed) {
            settings.set_strv('keybinding-toggle-popover', [PRESET_TOGGLE_SHORTCUT]);
            showToast(_('Global shortcut set to Super+Shift+A'));
        }
        closeDialog();
    });

    actionsBox.append(cancelBtn);
    actionsBox.append(clearBtn);
    actionsBox.append(presetBtn);

    contentBox.append(headingLabel);
    contentBox.append(hintLabel);
    contentBox.append(previewPill);
    contentBox.append(statusLabel);
    contentBox.append(actionsBox);
    dialog.set_child(contentBox);

    const keyController = new Gtk.EventControllerKey({
        name: 'agent-status-shortcut-capture',
        propagation_phase: Gtk.PropagationPhase.CAPTURE,
    });

    const modMask =
        Gdk.ModifierType.CONTROL_MASK |
        Gdk.ModifierType.SHIFT_MASK |
        Gdk.ModifierType.ALT_MASK |
        Gdk.ModifierType.SUPER_MASK |
        Gdk.ModifierType.HYPER_MASK |
        Gdk.ModifierType.META_MASK;
    const primaryModMask =
        Gdk.ModifierType.CONTROL_MASK |
        Gdk.ModifierType.ALT_MASK |
        Gdk.ModifierType.SUPER_MASK |
        Gdk.ModifierType.HYPER_MASK |
        Gdk.ModifierType.META_MASK;

    keyController.connect('key-pressed', (_ctrl, keyval, _keycode, state) => {
        if (cli.isDestroyed || closed)
            return Gdk.EVENT_STOP;

        const cleanMods = state & modMask;

        if (keyval === Gdk.KEY_Escape) {
            closeDialog();
            return Gdk.EVENT_STOP;
        }

        if (isStandaloneModifierKey(keyval))
            return Gdk.EVENT_STOP;

        // Allow bare Tab / Enter / Space navigation across the dialog buttons when no primary modifier is held
        if (
            (cleanMods & primaryModMask) === 0 &&
            (keyval === Gdk.KEY_Tab ||
                keyval === Gdk.KEY_ISO_Left_Tab ||
                keyval === Gdk.KEY_Return ||
                keyval === Gdk.KEY_KP_Enter ||
                keyval === Gdk.KEY_space)
        ) {
            return Gdk.EVENT_PROPAGATE;
        }

        // Bare BackSpace / Delete clears the shortcut cleanly (matching GNOME Settings)
        if (
            cleanMods === 0 &&
            (keyval === Gdk.KEY_BackSpace || keyval === Gdk.KEY_Delete)
        ) {
            settings.set_strv('keybinding-toggle-popover', []);
            showToast(_('Global toggle shortcut cleared'));
            closeDialog();
            return Gdk.EVENT_STOP;
        }

        let normKeyval = Gdk.keyval_to_lower(keyval);
        let normMods = cleanMods;
        if (keyval === Gdk.KEY_ISO_Left_Tab) {
            normKeyval = Gdk.KEY_Tab;
            normMods |= Gdk.ModifierType.SHIFT_MASK;
        }

        const isFunctionKey = normKeyval >= Gdk.KEY_F1 && normKeyval <= Gdk.KEY_F35;
        const hasPrimaryMod = (normMods & primaryModMask) !== 0;

        if ((!hasPrimaryMod && !isFunctionKey) || !Gtk.accelerator_valid(normKeyval, normMods)) {
            statusLabel.set_label(
                _('Include Super, Ctrl, or Alt (or press a function key F1–F12).')
            );
            return Gdk.EVENT_STOP;
        }

        const accelName = Gtk.accelerator_name(normKeyval, normMods);
        if (!accelName)
            return Gdk.EVENT_STOP;

        const readable = Gtk.accelerator_get_label(normKeyval, normMods) || accelName;
        settings.set_strv('keybinding-toggle-popover', [accelName]);
        showToast(_('Global shortcut set to %s').replace('%s', readable));
        closeDialog();
        return Gdk.EVENT_STOP;
    });

    dialog.add_controller(keyController);
    dialog.present();
    return dialog;
}

/**
 * Checks asynchronously whether `filePath` is an executable file.
 *
 * @param {string} filePath
 * @param {Gio.Cancellable|null} cancellable
 * @returns {Promise<boolean>}
 */
function isExecutableAsync(filePath, cancellable) {
    if (!filePath)
        return Promise.resolve(false);

    const file = Gio.File.new_for_path(filePath);
    return new Promise(resolve => {
        file.query_info_async(
            'standard::type,access::can-execute',
            Gio.FileQueryInfoFlags.NONE,
            GLib.PRIORITY_DEFAULT,
            cancellable,
            (src, res) => {
                try {
                    const info = src.query_info_finish(res);
                    const fileType = info.get_file_type();
                    const okType = fileType === Gio.FileType.REGULAR ||
                        fileType === Gio.FileType.SYMBOLIC_LINK;
                    resolve(Boolean(okType && info.get_attribute_boolean('access::can-execute')));
                } catch (_e) {
                    resolve(false);
                }
            }
        );
    });
}

/**
 * Helper binding a string GSetting to an `Adw.ComboRow` over `{id, label}` choices,
 * registering deterministic signal disconnection on `cli` when the window closes.
 *
 * @param {{
 *   title: string,
 *   subtitle?: string,
 *   options: ReadonlyArray<{id: string, label: string}>,
 *   settings: Gio.Settings,
 *   key: string,
 *   defaultId?: string,
 *   cli: PrefsCliRunner
 * }} params
 * @returns {Adw.ComboRow}
 */
function createBoundComboRow({title, subtitle, options, settings, key, defaultId, cli}) {
    const hasKey = hasSettingKey(settings, key);
    const stringList = Gtk.StringList.new(options.map(o => o.label));
    const row = new Adw.ComboRow({
        title: escapeMarkup(title),
        subtitle: subtitle ? escapeMarkup(subtitle) : '',
        model: stringList,
        sensitive: hasKey && options.length > 0,
    });

    const fallbackIdx = defaultId
        ? Math.max(0, options.findIndex(o => o.id === defaultId))
        : 0;

    if (!hasKey || options.length === 0) {
        if (options.length > 0)
            row.set_selected(fallbackIdx);
        return row;
    }

    let updating = false;
    const syncFromSettings = () => {
        if (cli.isDestroyed)
            return;
        const val = settings.get_string(key);
        const idx = options.findIndex(o => o.id === val);
        updating = true;
        row.set_selected(idx >= 0 ? idx : fallbackIdx);
        updating = false;
    };

    syncFromSettings();

    row.connect('notify::selected', () => {
        if (updating || cli.isDestroyed)
            return;
        const chosen = options[row.get_selected()];
        if (chosen && settings.get_string(key) !== chosen.id)
            settings.set_string(key, chosen.id);
    });

    let sigId = settings.connect(`changed::${key}`, () => {
        syncFromSettings();
    });
    const disconnectSetting = () => {
        if (sigId !== 0) {
            settings.disconnect(sigId);
            sigId = 0;
        }
    };
    cli.trackCleanup(disconnectSetting);
    row.connect('destroy', disconnectSetting);

    return row;
}

/**
 * Async CLI runner and deterministic lifecycle cleanup tracker scoped to the Preferences window.
 */
export class PrefsCliRunner {
    constructor(settings) {
        this._settings = settings;
        this._destroyed = false;
        this._resolvedBin = null;
        this._activeProcs = new Set();
        this._cancellables = new Set();
        this._cleanups = [];
        this._mutationChain = Promise.resolve();

        this._binSigId = hasSettingKey(this._settings, 'binary-path')
            ? this._settings.connect('changed::binary-path', () => {
                this._resolvedBin = null;
            })
            : 0;
    }

    get isDestroyed() {
        return this._destroyed;
    }

    get activeSubprocessCount() {
        return this._activeProcs.size;
    }

    get activeCancellableCount() {
        return this._cancellables.size;
    }

    /**
     * Registers a cleanup callback executed deterministically when the Preferences window closes.
     *
     * @param {() => void} fn
     */
    trackCleanup(fn) {
        if (this._destroyed) {
            try {
                fn();
            } catch (_e) {
                // Ignore late cleanup errors
            }
            return;
        }
        this._cleanups.push(fn);
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;

        if (this._binSigId !== 0) {
            this._settings.disconnect(this._binSigId);
            this._binSigId = 0;
        }

        while (this._cleanups.length > 0) {
            const fn = this._cleanups.pop();
            try {
                fn();
            } catch (_e) {
                // Ignore individual widget/settings cleanup errors
            }
        }

        for (const c of this._cancellables) {
            try {
                c.cancel();
            } catch (_e) {
                // Ignore cancellation cleanup errors
            }
        }
        this._cancellables.clear();

        for (const proc of this._activeProcs) {
            try {
                proc.force_exit();
            } catch (_e) {
                // Ignore already-exited subprocess errors
            }
        }
        this._activeProcs.clear();
    }

    newCancellable() {
        const c = new Gio.Cancellable();
        if (this._destroyed)
            c.cancel();
        else
            this._cancellables.add(c);
        return c;
    }

    releaseCancellable(c) {
        this._cancellables.delete(c);
    }

    /**
     * Serializes `config` file mutations so rapid UI changes never race atomic `config.toml` writes.
     *
     * @template T
     * @param {() => Promise<T>} taskFn
     * @returns {Promise<T|null>}
     */
    enqueueConfigMutation(taskFn) {
        const run = async () => {
            if (this._destroyed)
                return null;
            return await taskFn();
        };
        const next = this._mutationChain.then(run, run);
        this._mutationChain = next.then(
            () => undefined,
            () => undefined
        );
        return next;
    }

    async resolveBinaryAsync() {
        if (this._resolvedBin)
            return this._resolvedBin;

        const cancellable = this.newCancellable();
        try {
            const custom = hasSettingKey(this._settings, 'binary-path')
                ? this._settings.get_string('binary-path').trim()
                : '';
            if (custom !== '') {
                const expanded = custom.startsWith('~/')
                    ? GLib.build_filenamev([GLib.get_home_dir(), custom.slice(2)])
                    : custom;
                this._resolvedBin = expanded;
                return expanded;
            }

            const home = GLib.get_home_dir();
            const candidates = [
                GLib.build_filenamev([home, '.local', 'bin', BINARY_NAME]),
                GLib.build_filenamev([home, '.cargo', 'bin', BINARY_NAME]),
                `/usr/local/bin/${BINARY_NAME}`,
                `/usr/bin/${BINARY_NAME}`,
            ];

            const pathEnv = GLib.getenv('PATH') || '';
            for (const dir of pathEnv.split(':')) {
                if (!dir)
                    continue;
                const full = GLib.build_filenamev([dir, BINARY_NAME]);
                if (!candidates.includes(full))
                    candidates.push(full);
            }

            for (const candidate of candidates) {
                if (this._destroyed)
                    return BINARY_NAME;
                if (await isExecutableAsync(candidate, cancellable)) {
                    this._resolvedBin = candidate;
                    return candidate;
                }
            }

            this._resolvedBin = BINARY_NAME;
            return BINARY_NAME;
        } finally {
            this.releaseCancellable(cancellable);
        }
    }

    async runRawAsync(args) {
        if (this._destroyed)
            return null;

        const bin = await this.resolveBinaryAsync();
        if (this._destroyed)
            return null;

        const cancellable = this.newCancellable();
        let proc = null;

        try {
            proc = Gio.Subprocess.new(
                [bin, ...args],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE
            );
        } catch (e) {
            this.releaseCancellable(cancellable);
            this._resolvedBin = null;
            throw new Error(`Unable to execute ${bin}: ${e.message}`);
        }

        this._activeProcs.add(proc);
        try {
            let stdout = '';
            let stderr = '';
            try {
                [stdout, stderr] = await new Promise((resolve, reject) => {
                    proc.communicate_utf8_async(null, cancellable, (src, res) => {
                        try {
                            const [, out, err] = src.communicate_utf8_finish(res);
                            resolve([out ?? '', err ?? '']);
                        } catch (err) {
                            reject(err);
                        }
                    });
                });
            } catch (err) {
                if (this._destroyed || err?.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                    return null;
                throw err;
            }

            if (this._destroyed)
                return null;

            if (!proc.get_successful())
                throw new Error(stderr.trim() || `${BINARY_NAME} exited with an error`);

            return stdout;
        } finally {
            this._activeProcs.delete(proc);
            this.releaseCancellable(cancellable);
        }
    }

    async runJsonAsync(args) {
        const stdout = await this.runRawAsync(args);
        if (stdout === null)
            return null;
        const trimmed = stdout.trim();
        return trimmed ? JSON.parse(trimmed) : {};
    }

    /**
     * Triggers a background `collect --force` run after pending `config.toml` mutations settle
     * so `$XDG_CACHE_HOME/gnome-agent-status/state.json` (`0600`) updates immediately and
     * wakes `BackendClient`'s `Gio.FileMonitor` in the live extension.
     *
     * @returns {Promise<object|null>}
     */
    triggerBackgroundCollect() {
        if (this._destroyed)
            return Promise.resolve(null);

        return this._mutationChain
            .then(() => {
                if (this._destroyed)
                    return null;
                return this.runJsonAsync(['collect', '--force']);
            })
            .catch(() => {
                // Non-fatal background cache refresh after config mutation
                return null;
            });
    }
}

export default class AgentStatusPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const _ = typeof this.gettext === 'function'
            ? s => this.gettext(s)
            : s => String(s ?? '');
        const settings = this.getSettings();
        const cli = new PrefsCliRunner(settings);

        window.set_default_size(700, 740);
        window.set_search_enabled(true);

        window.connect('close-request', () => {
            cli.destroy();
            return false;
        });

        const showToast = message => {
            if (!message || cli.isDestroyed)
                return;
            try {
                window.add_toast(new Adw.Toast({title: escapeMarkup(message)}));
            } catch (_e) {
                // Ignore if window is closing
            }
        };

        window.add(this._buildAppearancePage(window, settings, cli, showToast, _));
        const refreshHooks = this._buildRefreshVisibilityPage(window, settings, cli, showToast, _);
        window.add(refreshHooks.page);
        const accountsHooks = this._buildAccountsPage(
            window,
            cli,
            showToast,
            _,
            cfg => refreshHooks.applyConfigState(cfg)
        );
        window.add(accountsHooks.page);

        // Load initial `gnome-agent-status config show` asynchronously
        void (async () => {
            try {
                const cfg = await cli.runJsonAsync(['config', 'show']);
                if (!cfg || cli.isDestroyed)
                    return;
                refreshHooks.applyConfigState(cfg);
                accountsHooks.applyConfigState(cfg);
            } catch (e) {
                if (!cli.isDestroyed)
                    accountsHooks.showLoadError(e.message);
            }
        })();
    }

    _buildAppearancePage(window, settings, cli, showToast, _) {
        const page = new Adw.PreferencesPage({
            title: _('Appearance & Top Bar'),
            icon_name: 'preferences-desktop-appearance-symbolic',
        });

        // Top Bar Indicator Group
        const panelGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Top Bar Indicator')),
            description: escapeMarkup(_('Configure how AI agent usage appears in the GNOME top panel.')),
        });

        panelGroup.add(createBoundComboRow({
            title: _('Panel Mode'),
            subtitle: _('Minimal single icon, compact provider pills, or full text summary'),
            options: PANEL_MODES.map(o => ({id: o.id, label: _(o.label)})),
            settings,
            key: 'panel-mode',
            defaultId: 'icon',
            cli,
        }));

        panelGroup.add(createBoundComboRow({
            title: _('Panel Box Alignment'),
            subtitle: _('Section of the GNOME top bar where the indicator sits'),
            options: PANEL_BOXES.map(o => ({id: o.id, label: _(o.label)})),
            settings,
            key: 'panel-box',
            defaultId: 'right',
            cli,
        }));

        const posRow = new Adw.SpinRow({
            title: escapeMarkup(_('Panel Position Index')),
            subtitle: escapeMarkup(_('Zero-based slot index within the chosen top bar box')),
            adjustment: new Gtk.Adjustment({
                lower: 0,
                upper: 25,
                step_increment: 1,
                page_increment: 5,
                value: settings.get_int('panel-position'),
            }),
        });
        settings.bind('panel-position', posRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        cli.trackCleanup(() => Gio.Settings.unbind(posRow, 'value'));
        panelGroup.add(posRow);

        const autoHideRow = new Adw.SwitchRow({
            title: escapeMarkup(_('Auto-Hide When Empty')),
            subtitle: escapeMarkup(
                _('Hide the top bar icon automatically when no active providers are detected')
            ),
        });
        settings.bind('auto-hide-when-empty', autoHideRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        cli.trackCleanup(() => Gio.Settings.unbind(autoHideRow, 'active'));
        panelGroup.add(autoHideRow);

        // Global Toggle Shortcut Row
        const hasShortcutKey = hasSettingKey(settings, 'keybinding-toggle-popover');
        const shortcutRow = new Adw.ActionRow({
            title: escapeMarkup(_('Global Toggle Shortcut')),
            subtitle: escapeMarkup(
                _('Toggle the usage popover from anywhere in GNOME Shell (optional)')
            ),
            sensitive: hasShortcutKey,
        });

        const shortcutPill = new Gtk.ShortcutLabel({
            accelerator: '',
            disabled_text: _('Disabled'),
            valign: Gtk.Align.CENTER,
        });

        const setShortcutBtn = new Gtk.Button({
            label: _('Set Shortcut…'),
            tooltip_text: _('Configure or clear the global keyboard shortcut'),
            valign: Gtk.Align.CENTER,
            sensitive: hasShortcutKey,
        });
        setShortcutBtn.connect('clicked', () => {
            openShortcutCaptureDialog({
                window,
                settings,
                cli,
                showToast,
                translate: _,
            });
        });

        const syncShortcutRow = () => {
            if (cli.isDestroyed)
                return;
            const parsed = hasShortcutKey
                ? parseShortcutSetting(settings.get_strv('keybinding-toggle-popover'))
                : {accel: '', readable: ''};
            shortcutPill.set_accelerator(parsed.accel);
            shortcutRow.set_subtitle(
                escapeMarkup(
                    parsed.readable
                        ? _('Active shortcut: %s · Click Set Shortcut… to change or clear').replace(
                            '%s',
                            parsed.readable
                        )
                        : _('Disabled · Click Set Shortcut… to bind (for example Super+Shift+A)')
                )
            );
        };

        syncShortcutRow();

        if (hasShortcutKey) {
            let shortcutSigId = settings.connect('changed::keybinding-toggle-popover', () => {
                syncShortcutRow();
            });
            const disconnectShortcutSig = () => {
                if (shortcutSigId !== 0) {
                    settings.disconnect(shortcutSigId);
                    shortcutSigId = 0;
                }
            };
            cli.trackCleanup(disconnectShortcutSig);
            shortcutRow.connect('destroy', disconnectShortcutSig);
        }

        shortcutRow.add_suffix(shortcutPill);
        shortcutRow.add_suffix(setShortcutBtn);
        panelGroup.add(shortcutRow);

        page.add(panelGroup);

        // Popover Theme & Layout Group
        const themeGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Popover Theme & Layout')),
            description: escapeMarkup(
                _('Customize curated palettes, card grid layout, density, and number formatting.')
            ),
        });

        themeGroup.add(createBoundComboRow({
            title: _('Theme Preset'),
            subtitle: _(
                'Synced live with the popover theme picker across Glass, Light, and Dark palettes'
            ),
            options: buildThemeComboOptions(THEME_PRESETS, _),
            settings,
            key: 'theme',
            defaultId: 'midnight-slate',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: _('Popover Layout'),
            subtitle: _(
                'Single column with Top 5 progressive disclosure or wide 2-column bento grid'
            ),
            options: POPOVER_LAYOUTS.map(o => ({id: o.id, label: _(o.label)})),
            settings,
            key: 'popover-layout',
            defaultId: 'list',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: _('Default Popover Density'),
            subtitle: _('Detailed multi-window breakdown or compact single-line summary'),
            options: DENSITY_MODES.map(o => ({id: o.id, label: _(o.label)})),
            settings,
            key: 'density-mode',
            defaultId: 'detailed',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: _('Font Style'),
            subtitle: _('System UI font with tabular numbers or developer monospace font'),
            options: FONT_STYLES.map(o => ({id: o.id, label: _(o.label)})),
            settings,
            key: 'font-style',
            defaultId: 'system',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: _('Quota Metric'),
            subtitle: _('Display percentages and gauges as quota used or quota remaining'),
            options: METRIC_MODES.map(o => ({id: o.id, label: _(o.label)})),
            settings,
            key: 'metric-mode',
            defaultId: 'used',
            cli,
        }));

        page.add(themeGroup);
        return page;
    }

    _buildRefreshVisibilityPage(window, settings, cli, showToast, _) {
        const page = new Adw.PreferencesPage({
            title: _('Refresh & Visibility'),
            icon_name: 'preferences-system-time-symbolic',
        });

        const cadenceGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Collection Cadence & Binary')),
            description: escapeMarkup(
                _('Control background polling intervals, discovery defaults, and binary path.')
            ),
        });

        const refreshSpin = new Adw.SpinRow({
            title: escapeMarkup(_('Background Refresh Interval (seconds)')),
            subtitle: escapeMarkup(
                _('Minimum 300s recommended to stay well within provider OAuth rate limits')
            ),
            adjustment: new Gtk.Adjustment({
                lower: 60,
                upper: 3600,
                step_increment: 30,
                page_increment: 60,
                value: Math.max(60, settings.get_int('refresh-interval-seconds') || 300),
            }),
        });
        settings.bind('refresh-interval-seconds', refreshSpin, 'value', Gio.SettingsBindFlags.DEFAULT);
        cli.trackCleanup(() => Gio.Settings.unbind(refreshSpin, 'value'));
        cadenceGroup.add(refreshSpin);

        let syncingConfig = false;

        const autoDiscoverRow = new Adw.SwitchRow({
            title: escapeMarkup(_('Auto-Discover Local Accounts')),
            subtitle: escapeMarkup(
                _(
                    'Automatically detect local Claude, Codex, Grok, Cursor, Antigravity, OpenRouter, Copilot, Gemini, Windsurf, and DeepSeek profiles'
                )
            ),
            active: true,
        });
        autoDiscoverRow.connect('notify::active', () => {
            if (syncingConfig || cli.isDestroyed)
                return;
            const flag = autoDiscoverRow.get_active() ? 'true' : 'false';
            void (async () => {
                try {
                    const updated = await cli.enqueueConfigMutation(() =>
                        cli.runJsonAsync(['config', 'set', '--auto-discover', flag])
                    );
                    if (!updated || cli.isDestroyed)
                        return;
                    void cli.triggerBackgroundCollect();
                    showToast(_('Updated auto-discovery setting'));
                } catch (e) {
                    if (!cli.isDestroyed)
                        showToast(_('Failed to update config.toml: %s').replace('%s', e.message));
                }
            })();
        });
        cadenceGroup.add(autoDiscoverRow);

        const timeoutSpin = new Adw.SpinRow({
            title: escapeMarkup(_('Collector Timeout (seconds)')),
            subtitle: escapeMarkup(_('Maximum wait time per collection run in config.toml')),
            adjustment: new Gtk.Adjustment({
                lower: 5,
                upper: 180,
                step_increment: 5,
                page_increment: 15,
                value: 30,
            }),
        });

        let timeoutDebounceId = 0;
        const clearTimeoutDebounce = () => {
            if (timeoutDebounceId !== 0) {
                GLib.Source.remove(timeoutDebounceId);
                timeoutDebounceId = 0;
            }
        };
        cli.trackCleanup(clearTimeoutDebounce);

        timeoutSpin.connect('notify::value', () => {
            if (syncingConfig || cli.isDestroyed)
                return;
            const secs = String(Math.round(timeoutSpin.get_value()));
            clearTimeoutDebounce();
            timeoutDebounceId = GLib.timeout_add(
                GLib.PRIORITY_DEFAULT,
                TIMEOUT_SPIN_DEBOUNCE_MS,
                () => {
                    timeoutDebounceId = 0;
                    if (cli.isDestroyed)
                        return GLib.SOURCE_REMOVE;
                    void (async () => {
                        try {
                            await cli.enqueueConfigMutation(() =>
                                cli.runJsonAsync(['config', 'set', '--timeout-seconds', secs])
                            );
                        } catch (e) {
                            if (!cli.isDestroyed)
                                showToast(_('Failed to update timeout: %s').replace('%s', e.message));
                        }
                    })();
                    return GLib.SOURCE_REMOVE;
                }
            );
            GLib.Source.set_name_by_id(timeoutDebounceId, '[gnome-agent-status] prefs-timeout-debounce');
        });
        cadenceGroup.add(timeoutSpin);

        // Custom binary override with two-way GSettings sync and Gtk.FileDialog
        const binaryEntry = new Adw.EntryRow({
            title: escapeMarkup(_('Custom gnome-agent-status Binary Path (Optional)')),
            text: settings.get_string('binary-path') || '',
        });
        let syncingBinaryEntry = false;
        binaryEntry.connect('changed', () => {
            if (syncingBinaryEntry || cli.isDestroyed)
                return;
            const val = binaryEntry.get_text().trim();
            if (settings.get_string('binary-path') !== val)
                settings.set_string('binary-path', val);
        });
        let binEntrySigId = settings.connect('changed::binary-path', () => {
            if (cli.isDestroyed)
                return;
            const nextVal = settings.get_string('binary-path') || '';
            if (binaryEntry.get_text().trim() !== nextVal) {
                syncingBinaryEntry = true;
                binaryEntry.set_text(nextVal);
                syncingBinaryEntry = false;
            }
        });
        const disconnectBinEntry = () => {
            if (binEntrySigId !== 0) {
                settings.disconnect(binEntrySigId);
                binEntrySigId = 0;
            }
        };
        cli.trackCleanup(disconnectBinEntry);
        binaryEntry.connect('destroy', disconnectBinEntry);

        const browseBinBtn = new Gtk.Button({
            label: _('Browse…'),
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        browseBinBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: _('Select gnome-agent-status Executable'),
                selectFolder: false,
                showToast,
                translate: _,
                onPathSelected: path => binaryEntry.set_text(path),
            });
        });
        binaryEntry.add_suffix(browseBinBtn);
        cadenceGroup.add(binaryEntry);

        page.add(cadenceGroup);

        // Desktop Notifications Group
        const notificationsGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Desktop Notifications')),
            description: escapeMarkup(
                _('Alert once per quota window when active providers cross usage thresholds or reset.')
            ),
        });

        const notificationSpecs = [
            {
                key: 'notify-on-warning',
                title: _('Warning Threshold (80%)'),
                subtitle: _(
                    'Send a calm notification once per window when a primary bucket reaches 80% used'
                ),
                fallback: false,
            },
            {
                key: 'notify-on-critical',
                title: _('Critical Threshold (90%)'),
                subtitle: _(
                    'Send an urgent notification once per window when a primary bucket reaches 90% used'
                ),
                fallback: true,
            },
            {
                key: 'notify-on-reset',
                title: _('Quota Reset Notification'),
                subtitle: _(
                    'Notify when a previously high-usage primary bucket resets back to fresh capacity'
                ),
                fallback: true,
            },
        ];

        for (const spec of notificationSpecs) {
            const hasKey = hasSettingKey(settings, spec.key);
            const notifyRow = new Adw.SwitchRow({
                title: escapeMarkup(spec.title),
                subtitle: escapeMarkup(spec.subtitle),
                active: hasKey ? settings.get_boolean(spec.key) : spec.fallback,
                sensitive: hasKey,
            });
            if (hasKey) {
                settings.bind(spec.key, notifyRow, 'active', Gio.SettingsBindFlags.DEFAULT);
                cli.trackCleanup(() => Gio.Settings.unbind(notifyRow, 'active'));
            }
            notificationsGroup.add(notifyRow);
        }

        page.add(notificationsGroup);

        // Unified Visible Agents & Display Order Group
        const agentsGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Visible Agents & Display Order')),
            description: escapeMarkup(
                _(
                    'The top 5 enabled agents appear on the main card list by default (with "Show more" or 2-Column layout when 6+ are enabled). Toggle visibility or move agents up and down to customize priority.'
                )
            ),
        });

        const hasOrderKey = hasSettingKey(settings, 'provider-order');
        const hasHiddenKey = hasSettingKey(settings, 'hidden-providers');
        const defaultOrder = resolveDefaultProviderOrder();
        const defaultHiddenSet = new Set(DEFAULT_HIDDEN_PROVIDERS);

        const readNormalizedOrder = () =>
            normalizeProviderOrder(hasOrderKey ? settings.get_strv('provider-order') : defaultOrder);
        const readHiddenSet = () =>
            new Set(
                (hasHiddenKey ? settings.get_strv('hidden-providers') : DEFAULT_HIDDEN_PROVIDERS)
                    .map(s => String(s ?? '').trim())
                    .filter(id => Boolean(id && PREFS_PROVIDER_BY_ID[id]))
            );

        const resetTop5Btn = new Gtk.Button({
            label: _('Reset to Default Top 5'),
            tooltip_text: _(
                'Restore default Top 5 visible agents (Claude, Codex, Grok, Cursor, Antigravity) and canonical display order'
            ),
            valign: Gtk.Align.CENTER,
            has_frame: false,
            css_classes: ['flat'],
        });
        agentsGroup.set_header_suffix(resetTop5Btn);

        let syncingAgentRows = false;
        const orderSlots = [];

        for (let idx = 0; idx < PREFS_PROVIDERS.length; idx++) {
            const initialProv = PREFS_PROVIDERS[idx];
            const row = new Adw.ActionRow({
                title: escapeMarkup(initialProv.displayName),
                subtitle: escapeMarkup(`Position ${idx + 1} · ID: ${initialProv.id}`),
            });

            const upBtn = new Gtk.Button({
                icon_name: 'go-up-symbolic',
                valign: Gtk.Align.CENTER,
                has_frame: false,
                css_classes: ['flat'],
            });

            const downBtn = new Gtk.Button({
                icon_name: 'go-down-symbolic',
                valign: Gtk.Align.CENTER,
                has_frame: false,
                css_classes: ['flat'],
            });

            const visSwitch = new Gtk.Switch({
                valign: Gtk.Align.CENTER,
                sensitive: hasHiddenKey,
            });

            const slot = {
                row,
                upBtn,
                downBtn,
                visSwitch,
                currentProvId: initialProv.id,
            };

            upBtn.connect('clicked', () => {
                if (cli.isDestroyed || !hasOrderKey || idx <= 0)
                    return;
                const hadFocus = upBtn.has_focus;
                const nextOrder = moveProviderInOrder(readNormalizedOrder(), idx, -1);
                settings.set_strv('provider-order', nextOrder);
                if (hadFocus && orderSlots[idx - 1]) {
                    const targetSlot = orderSlots[idx - 1];
                    if (idx - 1 > 0)
                        targetSlot.upBtn.grab_focus();
                    else
                        targetSlot.downBtn.grab_focus();
                }
            });

            downBtn.connect('clicked', () => {
                if (cli.isDestroyed || !hasOrderKey || idx >= PREFS_PROVIDERS.length - 1)
                    return;
                const hadFocus = downBtn.has_focus;
                const nextOrder = moveProviderInOrder(readNormalizedOrder(), idx, 1);
                settings.set_strv('provider-order', nextOrder);
                if (hadFocus && orderSlots[idx + 1]) {
                    const targetSlot = orderSlots[idx + 1];
                    if (idx + 1 < PREFS_PROVIDERS.length - 1)
                        targetSlot.downBtn.grab_focus();
                    else
                        targetSlot.upBtn.grab_focus();
                }
            });

            visSwitch.connect('notify::active', () => {
                if (syncingAgentRows || cli.isDestroyed || !hasHiddenKey)
                    return;
                const provId = slot.currentProvId;
                if (!provId)
                    return;
                const nextHidden = new Set(settings.get_strv('hidden-providers') || []);
                if (visSwitch.get_active())
                    nextHidden.delete(provId);
                else
                    nextHidden.add(provId);
                settings.set_strv('hidden-providers', Array.from(nextHidden));
            });

            row.add_suffix(upBtn);
            row.add_suffix(downBtn);
            row.add_suffix(visSwitch);
            agentsGroup.add(row);
            orderSlots.push(slot);
        }

        const syncAgentRows = () => {
            if (cli.isDestroyed)
                return;
            const orderedIds = readNormalizedOrder();
            const hiddenSet = readHiddenSet();

            let enabledCounter = 0;
            syncingAgentRows = true;
            try {
                for (let idx = 0; idx < orderSlots.length; idx++) {
                    const slot = orderSlots[idx];
                    const provId = orderedIds[idx] ?? PREFS_PROVIDERS[idx].id;
                    const prov = PREFS_PROVIDER_BY_ID[provId] ?? {
                        id: provId,
                        displayName: provId,
                    };
                    const isVisible = !hiddenSet.has(provId);
                    let statusTag;
                    if (isVisible) {
                        enabledCounter++;
                        statusTag = enabledCounter <= 5
                            ? _('Main Card #%d (Top 5)').replace('%d', String(enabledCounter))
                            : _('Enabled #%d ("Show more" / 2-Column)').replace(
                                '%d',
                                String(enabledCounter)
                            );
                    } else {
                        statusTag = _('Hidden');
                    }

                    slot.currentProvId = provId;
                    slot.row.set_title(escapeMarkup(prov.displayName));
                    slot.row.set_subtitle(
                        escapeMarkup(
                            _('Position %d · %s · ID: %s')
                                .replace('%d', String(idx + 1))
                                .replace('%s', statusTag)
                                .replace('%s', prov.id)
                        )
                    );

                    const upLabel = _('Move %s up').replace('%s', prov.displayName);
                    const downLabel = _('Move %s down').replace('%s', prov.displayName);
                    const toggleLabel = _('Toggle %s visibility').replace('%s', prov.displayName);
                    slot.upBtn.set_tooltip_text(upLabel);
                    slot.downBtn.set_tooltip_text(downLabel);
                    slot.visSwitch.set_tooltip_text(toggleLabel);
                    slot.upBtn.update_property([Gtk.AccessibleProperty.LABEL], [upLabel]);
                    slot.downBtn.update_property([Gtk.AccessibleProperty.LABEL], [downLabel]);
                    slot.visSwitch.update_property([Gtk.AccessibleProperty.LABEL], [toggleLabel]);

                    slot.upBtn.set_sensitive(hasOrderKey && idx > 0);
                    slot.downBtn.set_sensitive(hasOrderKey && idx < orderSlots.length - 1);
                    if (slot.visSwitch.get_active() !== isVisible)
                        slot.visSwitch.set_active(isVisible);
                }
            } finally {
                syncingAgentRows = false;
            }

            const isDefaultOrder =
                orderedIds.length === defaultOrder.length &&
                orderedIds.every((id, i) => id === defaultOrder[i]);
            const isDefaultHidden =
                hiddenSet.size === defaultHiddenSet.size &&
                Array.from(defaultHiddenSet).every(id => hiddenSet.has(id));

            resetTop5Btn.set_sensitive(
                (hasOrderKey && !isDefaultOrder) || (hasHiddenKey && !isDefaultHidden)
            );
        };

        resetTop5Btn.connect('clicked', () => {
            if (cli.isDestroyed)
                return;
            if (hasOrderKey)
                settings.set_strv('provider-order', [...defaultOrder]);
            if (hasHiddenKey)
                settings.set_strv('hidden-providers', [...DEFAULT_HIDDEN_PROVIDERS]);
            showToast(_('Restored default Top 5 visible agents and display order'));
        });

        syncAgentRows();

        const signalDisconnectorIds = [];
        if (hasOrderKey) {
            signalDisconnectorIds.push(
                settings.connect('changed::provider-order', () => syncAgentRows())
            );
        }
        if (hasHiddenKey) {
            signalDisconnectorIds.push(
                settings.connect('changed::hidden-providers', () => syncAgentRows())
            );
        }

        const disconnectAgentSigs = () => {
            while (signalDisconnectorIds.length > 0) {
                const id = signalDisconnectorIds.pop();
                if (id)
                    settings.disconnect(id);
            }
        };
        cli.trackCleanup(disconnectAgentSigs);
        agentsGroup.connect('destroy', disconnectAgentSigs);

        page.add(agentsGroup);

        return {
            page,
            applyConfigState: cfg => {
                if (cli.isDestroyed)
                    return;
                syncingConfig = true;
                if (typeof cfg.auto_discover === 'boolean')
                    autoDiscoverRow.set_active(cfg.auto_discover);
                if (typeof cfg.timeout_seconds === 'number' && cfg.timeout_seconds >= 5)
                    timeoutSpin.set_value(cfg.timeout_seconds);
                syncingConfig = false;
            },
        };
    }

    _buildAccountsPage(window, cli, showToast, _, onReloadConfig = null) {
        const page = new Adw.PreferencesPage({
            title: _('Accounts & Manual Locations'),
            icon_name: 'system-users-symbolic',
        });

        // 1. Configured Manual Accounts Group
        const listGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Configured Manual Account Locations')),
            description: escapeMarkup(
                _('Manual directories, credential files, or databases stored in config.toml.')
            ),
        });

        const headerActionsBox = new Gtk.Box({
            orientation: Gtk.Orientation.HORIZONTAL,
            spacing: 6,
            valign: Gtk.Align.CENTER,
        });

        let currentConfigPath = GLib.build_filenamev([
            GLib.get_user_config_dir(),
            'gnome-agent-status',
            'config.toml',
        ]);
        let currentManualAccounts = [];
        let editingKey = null;

        const openConfigBtn = new Gtk.Button({
            label: _('Open config.toml'),
            css_classes: ['flat'],
        });
        openConfigBtn.connect('clicked', () => {
            if (cli.isDestroyed)
                return;
            void (async () => {
                const cancellable = cli.newCancellable();
                try {
                    const cfgFile = Gio.File.new_for_path(currentConfigPath);
                    const exists = await new Promise(resolve => {
                        cfgFile.query_info_async(
                            'standard::type',
                            Gio.FileQueryInfoFlags.NONE,
                            GLib.PRIORITY_DEFAULT,
                            cancellable,
                            (src, res) => {
                                try {
                                    src.query_info_finish(res);
                                    resolve(true);
                                } catch (_e) {
                                    resolve(false);
                                }
                            }
                        );
                    });
                    if (cli.isDestroyed)
                        return;
                    if (!exists)
                        await cli.runRawAsync(['--init-config']).catch(() => {});
                    if (cli.isDestroyed)
                        return;
                    const uri = GLib.filename_to_uri(currentConfigPath, null);
                    Gio.AppInfo.launch_default_for_uri(uri, null);
                } catch (e) {
                    if (!cli.isDestroyed)
                        showToast(_('Could not open config.toml: %s').replace('%s', e.message));
                } finally {
                    cli.releaseCancellable(cancellable);
                }
            })();
        });

        const reloadBtn = new Gtk.Button({
            icon_name: 'view-refresh-symbolic',
            tooltip_text: _('Reload manual accounts from config.toml'),
            css_classes: ['flat'],
        });
        headerActionsBox.append(openConfigBtn);
        headerActionsBox.append(reloadBtn);
        listGroup.set_header_suffix(headerActionsBox);

        const dynamicRows = [];
        const clearDynamicRows = () => {
            while (dynamicRows.length > 0) {
                const row = dynamicRows.pop();
                listGroup.remove(row);
            }
        };

        // 2. Add / Update Manual Account Form Group
        const formGroup = new Adw.PreferencesGroup({
            title: escapeMarkup(_('Add or Update Manual Account')),
            description: escapeMarkup(
                _(
                    'Point manually to any custom Claude, Codex, Grok, Cursor, Antigravity, OpenRouter, Copilot, Gemini, Windsurf, or DeepSeek directory or credential file if auto-discovery does not find it.'
                )
            ),
        });

        const providerCombo = new Adw.ComboRow({
            title: escapeMarkup(_('Provider')),
            subtitle: escapeMarkup(
                _('Example location: %s').replace('%s', PREFS_PROVIDERS[0].locationHint)
            ),
            model: Gtk.StringList.new(PREFS_PROVIDERS.map(p => p.displayName)),
        });

        const labelEntry = new Adw.EntryRow({
            title: escapeMarkup(_('Display Label (e.g. Work Claude)')),
        });

        const idEntry = new Adw.EntryRow({
            title: escapeMarkup(_('Account ID Slug (e.g. work — auto-filled if blank)')),
        });

        const pathEntry = new Adw.EntryRow({
            title: escapeMarkup(_('Profile Directory or Auth/DB File Path')),
        });

        const pickFolderBtn = new Gtk.Button({
            label: _('Folder…'),
            tooltip_text: _('Select a custom profile directory (e.g. ~/.claude-work or ~/.codex-work)'),
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickFolderBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: _('Select Custom Account Directory'),
                selectFolder: true,
                showToast,
                translate: _,
                onPathSelected: folderPath => pathEntry.set_text(folderPath),
            });
        });

        const pickFileBtn = new Gtk.Button({
            label: _('File…'),
            tooltip_text: _(
                'Select a specific credential or database file (e.g. state.vscdb, api_key, or auth.json)'
            ),
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickFileBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: _('Select Account Credential or Database File'),
                selectFolder: false,
                showToast,
                translate: _,
                onPathSelected: filePath => pathEntry.set_text(filePath),
            });
        });

        pathEntry.add_suffix(pickFolderBtn);
        pathEntry.add_suffix(pickFileBtn);

        const binaryOverrideEntry = new Adw.EntryRow({
            title: escapeMarkup(_('Custom Provider CLI Binary (Optional)')),
        });
        const pickBinBtn = new Gtk.Button({
            label: _('Browse…'),
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickBinBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: _('Select Provider CLI Binary'),
                selectFolder: false,
                showToast,
                translate: _,
                onPathSelected: binPath => binaryOverrideEntry.set_text(binPath),
            });
        });
        binaryOverrideEntry.add_suffix(pickBinBtn);

        const pollModeCombo = new Adw.ComboRow({
            title: escapeMarkup(_('Polling Mode')),
            subtitle: escapeMarkup(_('Active background polling or passive local cache updates only')),
            model: Gtk.StringList.new(POLL_MODE_OPTIONS.map(o => _(o.label))),
        });

        const switchCmdEntry = new Adw.EntryRow({
            title: escapeMarkup(_('External Switch Command (Optional)')),
        });

        providerCombo.connect('notify::selected', () => {
            if (cli.isDestroyed)
                return;
            const chosen = PREFS_PROVIDERS[providerCombo.get_selected()] ?? PREFS_PROVIDERS[0];
            providerCombo.set_subtitle(
                escapeMarkup(_('Example location: %s').replace('%s', chosen.locationHint))
            );
        });

        const resetForm = () => {
            editingKey = null;
            providerCombo.set_selected(0);
            labelEntry.set_text('');
            idEntry.set_text('');
            pathEntry.set_text('');
            binaryOverrideEntry.set_text('');
            pollModeCombo.set_selected(0);
            switchCmdEntry.set_text('');
        };

        const populateFormForEdit = acc => {
            editingKey = {
                provider: String(acc.provider),
                id: String(acc.id),
            };
            const pIdx = PREFS_PROVIDERS.findIndex(p => p.id === acc.provider);
            providerCombo.set_selected(pIdx >= 0 ? pIdx : 0);
            labelEntry.set_text(acc.label || '');
            idEntry.set_text(acc.id || '');
            pathEntry.set_text(acc.path || '');
            binaryOverrideEntry.set_text(acc.binary || '');
            pollModeCombo.set_selected(acc.poll_mode === 'passive' ? 1 : 0);
            switchCmdEntry.set_text(acc.switch_command || '');
            showToast(
                _('Loaded "%s" into editor below').replace('%s', String(acc.label || acc.id))
            );
        };

        const submitActionRow = new Adw.ActionRow({
            title: escapeMarkup(_('Save to config.toml')),
            subtitle: escapeMarkup(
                _('Writes [[providers.<id>.accounts]] safely and refreshes cached state')
            ),
        });

        const clearFormBtn = new Gtk.Button({
            label: _('Clear'),
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        clearFormBtn.connect('clicked', () => resetForm());

        const saveBtn = new Gtk.Button({
            label: _('Save Account'),
            valign: Gtk.Align.CENTER,
            css_classes: ['suggested-action'],
        });

        submitActionRow.add_suffix(clearFormBtn);
        submitActionRow.add_suffix(saveBtn);

        formGroup.add(providerCombo);
        formGroup.add(labelEntry);
        formGroup.add(idEntry);
        formGroup.add(pathEntry);
        formGroup.add(binaryOverrideEntry);
        formGroup.add(pollModeCombo);
        formGroup.add(switchCmdEntry);
        formGroup.add(submitActionRow);

        const renderManualAccounts = cfg => {
            if (cli.isDestroyed)
                return;
            clearDynamicRows();
            if (cfg?.config_path)
                currentConfigPath = cfg.config_path;

            const accounts = Array.isArray(cfg?.manual_accounts) ? cfg.manual_accounts : [];
            currentManualAccounts = accounts;
            if (accounts.length === 0) {
                const emptyRow = new Adw.ActionRow({
                    title: escapeMarkup(_('No manual accounts configured')),
                    subtitle: escapeMarkup(
                        _('Auto-discovery handles default locations. Use the form below to add custom paths.')
                    ),
                });
                listGroup.add(emptyRow);
                dynamicRows.push(emptyRow);
                return;
            }

            for (const acc of accounts) {
                const provMeta = PREFS_PROVIDER_BY_ID[acc.provider] ?? PROVIDER_BY_ID[acc.provider];
                const provName = provMeta?.displayName || acc.provider;
                const details = [];
                if (acc.path)
                    details.push(acc.path);
                if (acc.binary)
                    details.push(`bin: ${acc.binary}`);
                if (acc.poll_mode)
                    details.push(`mode: ${acc.poll_mode}`);

                const row = new Adw.ActionRow({
                    title: escapeMarkup(`${provName} · ${acc.label || acc.id} (${acc.id})`),
                    subtitle: escapeMarkup(details.join('  •  ') || _('Default path')),
                });

                const editBtn = new Gtk.Button({
                    icon_name: 'document-edit-symbolic',
                    tooltip_text: _('Edit manual account'),
                    valign: Gtk.Align.CENTER,
                    css_classes: ['flat'],
                });
                editBtn.connect('clicked', () => populateFormForEdit(acc));

                const removeBtn = new Gtk.Button({
                    icon_name: 'user-trash-symbolic',
                    tooltip_text: _('Remove manual account'),
                    valign: Gtk.Align.CENTER,
                    css_classes: ['flat', 'destructive-action'],
                });
                removeBtn.connect('clicked', () => {
                    if (cli.isDestroyed)
                        return;
                    removeBtn.set_sensitive(false);
                    void (async () => {
                        try {
                            const updated = await cli.enqueueConfigMutation(() =>
                                cli.runJsonAsync([
                                    'config',
                                    'remove-account',
                                    '--provider',
                                    String(acc.provider),
                                    '--id',
                                    String(acc.id),
                                ])
                            );
                            if (updated && !cli.isDestroyed) {
                                if (
                                    editingKey &&
                                    editingKey.provider === String(acc.provider) &&
                                    editingKey.id === String(acc.id)
                                ) {
                                    editingKey = null;
                                }
                                renderManualAccounts(updated);
                                void cli.triggerBackgroundCollect();
                                showToast(
                                    _('Removed manual account "%s"').replace(
                                        '%s',
                                        String(acc.label || acc.id)
                                    )
                                );
                            }
                        } catch (e) {
                            if (!cli.isDestroyed) {
                                removeBtn.set_sensitive(true);
                                showToast(
                                    _('Failed to remove account: %s').replace('%s', e.message)
                                );
                            }
                        }
                    })();
                });

                row.add_suffix(editBtn);
                row.add_suffix(removeBtn);
                listGroup.add(row);
                dynamicRows.push(row);
            }
        };

        const showLoadError = message => {
            if (cli.isDestroyed)
                return;
            clearDynamicRows();
            const errRow = new Adw.ActionRow({
                title: escapeMarkup(_('Could not query gnome-agent-status config')),
                subtitle: escapeMarkup(
                    message ||
                        _(
                            'Verify gnome-agent-status is installed or set Binary Path on Refresh & Visibility.'
                        )
                ),
            });
            listGroup.add(errRow);
            dynamicRows.push(errRow);
        };

        reloadBtn.connect('clicked', () => {
            if (cli.isDestroyed)
                return;
            void (async () => {
                try {
                    const cfg = await cli.runJsonAsync(['config', 'show']);
                    if (cfg && !cli.isDestroyed) {
                        onReloadConfig?.(cfg);
                        renderManualAccounts(cfg);
                    }
                } catch (e) {
                    if (!cli.isDestroyed)
                        showLoadError(e.message);
                }
            })();
        });

        saveBtn.connect('clicked', () => {
            if (cli.isDestroyed)
                return;
            const chosenProv = PREFS_PROVIDERS[providerCombo.get_selected()] ?? PREFS_PROVIDERS[0];
            const rawLabel = labelEntry.get_text().trim();
            const rawId = idEntry.get_text().trim();
            const rawPath = pathEntry.get_text().trim();
            const rawBin = binaryOverrideEntry.get_text().trim();
            const pollOpt = POLL_MODE_OPTIONS[pollModeCombo.get_selected()] ?? POLL_MODE_OPTIONS[0];
            const rawSwitch = switchCmdEntry.get_text().trim();

            if (!rawLabel && !rawId && !rawPath) {
                showToast(_('Enter at least a label or custom path before saving'));
                return;
            }

            const finalSlug = slugifyAccountId(rawId || rawLabel || `${chosenProv.id}-custom`);
            const finalLabel = rawLabel || `${chosenProv.displayName} (${finalSlug})`;

            const args = buildConfigUpsertArgs({
                provider: chosenProv.id,
                id: finalSlug,
                label: finalLabel,
                path: rawPath || null,
                binary: rawBin || null,
                pollMode: pollOpt.id,
                switchCommand: rawSwitch || null,
            });

            // When updating an existing account, explicitly pass empty strings for cleared optional fields
            // so `gnome-agent-status config upsert-account` removes stale keys from `config.toml`.
            const existingTarget = currentManualAccounts.find(
                a => a.provider === chosenProv.id && a.id === finalSlug
            );
            if (existingTarget) {
                if (!rawPath && existingTarget.path)
                    args.push('--path', '');
                if (!rawBin && existingTarget.binary)
                    args.push('--binary', '');
                if (!rawSwitch && existingTarget.switch_command)
                    args.push('--switch-command', '');
            }

            const priorEditKey = editingKey ? {...editingKey} : null;

            saveBtn.set_sensitive(false);
            void (async () => {
                try {
                    const updated = await cli.enqueueConfigMutation(async () => {
                        let summary = await cli.runJsonAsync(args);
                        if (
                            summary &&
                            priorEditKey &&
                            (priorEditKey.provider !== chosenProv.id || priorEditKey.id !== finalSlug)
                        ) {
                            summary = await cli.runJsonAsync([
                                'config',
                                'remove-account',
                                '--provider',
                                priorEditKey.provider,
                                '--id',
                                priorEditKey.id,
                            ]);
                        }
                        return summary;
                    });
                    if (updated && !cli.isDestroyed) {
                        renderManualAccounts(updated);
                        resetForm();
                        void cli.triggerBackgroundCollect();
                        showToast(_('Saved manual account "%s"').replace('%s', finalLabel));
                    }
                } catch (e) {
                    if (!cli.isDestroyed)
                        showToast(_('Failed to save manual account: %s').replace('%s', e.message));
                } finally {
                    if (!cli.isDestroyed)
                        saveBtn.set_sensitive(true);
                }
            })();
        });

        page.add(listGroup);
        page.add(formGroup);

        return {
            page,
            applyConfigState: renderManualAccounts,
            showLoadError,
        };
    }
}
