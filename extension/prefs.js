import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {
    CANONICAL_PROVIDERS,
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

const POLL_MODE_OPTIONS = Object.freeze([
    Object.freeze({id: 'active', label: 'Active Polling'}),
    Object.freeze({id: 'passive', label: 'Passive Cache Only'}),
]);

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
 * @returns {string|null}
 */
export function extractLocalDialogPath(file, showToast) {
    if (!file)
        return null;
    const localPath = file.get_path();
    if (!localPath || localPath.trim() === '') {
        showToast?.('Selected location must be a local filesystem path');
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
 *   onPathSelected: (localPath: string) => void
 * }} params
 */
function openLocalFileDialog({
    window,
    cli,
    title,
    selectFolder = false,
    showToast,
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
            showToast(`Could not select path: ${err.message}`);
            return;
        }

        if (cli.isDestroyed)
            return;

        const localPath = extractLocalDialogPath(chosenFile, showToast);
        if (localPath)
            onPathSelected(localPath);
    };

    if (selectFolder)
        dialog.select_folder(window, cancellable, onReady);
    else
        dialog.open(window, cancellable, onReady);
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
 *   cli: PrefsCliRunner
 * }} params
 * @returns {Adw.ComboRow}
 */
function createBoundComboRow({title, subtitle, options, settings, key, cli}) {
    const stringList = Gtk.StringList.new(options.map(o => o.label));
    const row = new Adw.ComboRow({
        title: escapeMarkup(title),
        subtitle: subtitle ? escapeMarkup(subtitle) : '',
        model: stringList,
    });

    let updating = false;
    const syncFromSettings = () => {
        if (cli.isDestroyed)
            return;
        const val = settings.get_string(key);
        const idx = options.findIndex(o => o.id === val);
        updating = true;
        row.set_selected(idx >= 0 ? idx : 0);
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

        this._binSigId = this._settings.connect('changed::binary-path', () => {
            this._resolvedBin = null;
        });
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
            const custom = this._settings.get_string('binary-path').trim();
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
        const settings = this.getSettings();
        const cli = new PrefsCliRunner(settings);

        window.set_default_size(680, 720);
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

        window.add(this._buildAppearancePage(settings, cli));
        const refreshHooks = this._buildRefreshVisibilityPage(window, settings, cli, showToast);
        window.add(refreshHooks.page);
        const accountsHooks = this._buildAccountsPage(
            window,
            cli,
            showToast,
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

    _buildAppearancePage(settings, cli) {
        const page = new Adw.PreferencesPage({
            title: 'Appearance & Top Bar',
            icon_name: 'preferences-desktop-appearance-symbolic',
        });

        // Top Bar Indicator Group
        const panelGroup = new Adw.PreferencesGroup({
            title: escapeMarkup('Top Bar Indicator'),
            description: escapeMarkup('Configure how AI agent usage appears in the GNOME top panel.'),
        });

        panelGroup.add(createBoundComboRow({
            title: 'Panel Mode',
            subtitle: 'Minimal single icon, compact provider pills, or full text summary',
            options: PANEL_MODES,
            settings,
            key: 'panel-mode',
            cli,
        }));

        panelGroup.add(createBoundComboRow({
            title: 'Panel Box Alignment',
            subtitle: 'Section of the GNOME top bar where the indicator sits',
            options: PANEL_BOXES,
            settings,
            key: 'panel-box',
            cli,
        }));

        const posRow = new Adw.SpinRow({
            title: escapeMarkup('Panel Position Index'),
            subtitle: escapeMarkup('Zero-based slot index within the chosen top bar box'),
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
            title: escapeMarkup('Auto-Hide When Empty'),
            subtitle: escapeMarkup('Hide the top bar icon automatically when no active providers are detected'),
        });
        settings.bind('auto-hide-when-empty', autoHideRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        cli.trackCleanup(() => Gio.Settings.unbind(autoHideRow, 'active'));
        panelGroup.add(autoHideRow);

        page.add(panelGroup);

        // Popover Theme & Typography Group
        const themeGroup = new Adw.PreferencesGroup({
            title: escapeMarkup('Popover Theme & Layout'),
            description: escapeMarkup('Customize colors, information density, and number formatting.'),
        });

        themeGroup.add(createBoundComboRow({
            title: 'Theme Preset',
            subtitle: 'Applies immediately to the expandable usage popover and gauges',
            options: THEME_PRESETS,
            settings,
            key: 'theme',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: 'Default Popover Density',
            subtitle: 'Detailed multi-window breakdown or compact single-line summary',
            options: DENSITY_MODES,
            settings,
            key: 'density-mode',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: 'Font Style',
            subtitle: 'System UI font with tabular numbers or developer monospace font',
            options: FONT_STYLES,
            settings,
            key: 'font-style',
            cli,
        }));

        themeGroup.add(createBoundComboRow({
            title: 'Quota Metric',
            subtitle: 'Display percentages and gauges as quota used or quota remaining',
            options: METRIC_MODES,
            settings,
            key: 'metric-mode',
            cli,
        }));

        page.add(themeGroup);
        return page;
    }

    _buildRefreshVisibilityPage(window, settings, cli, showToast) {
        const page = new Adw.PreferencesPage({
            title: 'Refresh & Visibility',
            icon_name: 'preferences-system-time-symbolic',
        });

        const cadenceGroup = new Adw.PreferencesGroup({
            title: escapeMarkup('Collection Cadence & Binary'),
            description: escapeMarkup('Control background polling intervals, discovery defaults, and binary path.'),
        });

        const refreshSpin = new Adw.SpinRow({
            title: escapeMarkup('Background Refresh Interval (seconds)'),
            subtitle: escapeMarkup('Minimum 300s recommended to stay well within provider OAuth rate limits'),
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
            title: escapeMarkup('Auto-Discover Local Accounts'),
            subtitle: escapeMarkup('Automatically detect local Claude, Codex, Grok, Antigravity, and Cursor profiles'),
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
                    showToast('Updated auto-discovery setting');
                } catch (e) {
                    if (!cli.isDestroyed)
                        showToast(`Failed to update config.toml: ${e.message}`);
                }
            })();
        });
        cadenceGroup.add(autoDiscoverRow);

        const timeoutSpin = new Adw.SpinRow({
            title: escapeMarkup('Collector Timeout (seconds)'),
            subtitle: escapeMarkup('Maximum wait time per collection run in config.toml'),
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
                                showToast(`Failed to update timeout: ${e.message}`);
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
            title: escapeMarkup('Custom gnome-agent-status Binary Path (Optional)'),
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
            label: 'Browse…',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        browseBinBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: 'Select gnome-agent-status Executable',
                selectFolder: false,
                showToast,
                onPathSelected: path => binaryEntry.set_text(path),
            });
        });
        binaryEntry.add_suffix(browseBinBtn);
        cadenceGroup.add(binaryEntry);

        page.add(cadenceGroup);

        // Provider Visibility Group
        const visibilityGroup = new Adw.PreferencesGroup({
            title: escapeMarkup('Visible Providers'),
            description: escapeMarkup('Choose which providers appear in the top bar and usage popover.'),
        });

        for (const prov of CANONICAL_PROVIDERS) {
            const hiddenInitial = new Set(settings.get_strv('hidden-providers') || []);
            const switchRow = new Adw.SwitchRow({
                title: escapeMarkup(prov.displayName),
                subtitle: escapeMarkup(`Provider ID: ${prov.id}`),
                active: !hiddenInitial.has(prov.id),
            });

            let internalSync = false;
            switchRow.connect('notify::active', () => {
                if (internalSync || cli.isDestroyed)
                    return;
                const currentHidden = new Set(settings.get_strv('hidden-providers') || []);
                if (switchRow.get_active())
                    currentHidden.delete(prov.id);
                else
                    currentHidden.add(prov.id);
                settings.set_strv('hidden-providers', Array.from(currentHidden));
            });

            let sigId = settings.connect('changed::hidden-providers', () => {
                if (cli.isDestroyed)
                    return;
                const updatedHidden = new Set(settings.get_strv('hidden-providers') || []);
                const shouldBeActive = !updatedHidden.has(prov.id);
                if (switchRow.get_active() !== shouldBeActive) {
                    internalSync = true;
                    switchRow.set_active(shouldBeActive);
                    internalSync = false;
                }
            });
            const disconnectSwitch = () => {
                if (sigId !== 0) {
                    settings.disconnect(sigId);
                    sigId = 0;
                }
            };
            cli.trackCleanup(disconnectSwitch);
            switchRow.connect('destroy', disconnectSwitch);

            visibilityGroup.add(switchRow);
        }

        page.add(visibilityGroup);

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

    _buildAccountsPage(window, cli, showToast, onReloadConfig = null) {
        const page = new Adw.PreferencesPage({
            title: 'Accounts & Manual Locations',
            icon_name: 'system-users-symbolic',
        });

        // 1. Configured Manual Accounts Group
        const listGroup = new Adw.PreferencesGroup({
            title: escapeMarkup('Configured Manual Account Locations'),
            description: escapeMarkup('Manual directories, credential files, or databases stored in config.toml.'),
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
            label: 'Open config.toml',
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
                        showToast(`Could not open config.toml: ${e.message}`);
                } finally {
                    cli.releaseCancellable(cancellable);
                }
            })();
        });

        const reloadBtn = new Gtk.Button({
            icon_name: 'view-refresh-symbolic',
            tooltip_text: 'Reload manual accounts from config.toml',
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
            title: escapeMarkup('Add or Update Manual Account'),
            description: escapeMarkup('Point manually to any custom Claude, Codex, Grok, Antigravity, or Cursor directory or file if auto-discovery does not find it.'),
        });

        const providerCombo = new Adw.ComboRow({
            title: escapeMarkup('Provider'),
            subtitle: escapeMarkup(`Example location: ${CANONICAL_PROVIDERS[0].locationHint}`),
            model: Gtk.StringList.new(CANONICAL_PROVIDERS.map(p => p.displayName)),
        });

        const labelEntry = new Adw.EntryRow({
            title: escapeMarkup('Display Label (e.g. Work Claude)'),
        });

        const idEntry = new Adw.EntryRow({
            title: escapeMarkup('Account ID Slug (e.g. work — auto-filled if blank)'),
        });

        const pathEntry = new Adw.EntryRow({
            title: escapeMarkup('Profile Directory or Auth/DB File Path'),
        });

        const pickFolderBtn = new Gtk.Button({
            label: 'Folder…',
            tooltip_text: 'Select a custom profile directory (e.g. ~/.claude-work or ~/.codex-work)',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickFolderBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: 'Select Custom Account Directory',
                selectFolder: true,
                showToast,
                onPathSelected: folderPath => pathEntry.set_text(folderPath),
            });
        });

        const pickFileBtn = new Gtk.Button({
            label: 'File…',
            tooltip_text: 'Select a specific credential or database file (e.g. state.vscdb or auth.json)',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickFileBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: 'Select Account Credential or Database File',
                selectFolder: false,
                showToast,
                onPathSelected: filePath => pathEntry.set_text(filePath),
            });
        });

        pathEntry.add_suffix(pickFolderBtn);
        pathEntry.add_suffix(pickFileBtn);

        const binaryOverrideEntry = new Adw.EntryRow({
            title: escapeMarkup('Custom Provider CLI Binary (Optional)'),
        });
        const pickBinBtn = new Gtk.Button({
            label: 'Browse…',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickBinBtn.connect('clicked', () => {
            openLocalFileDialog({
                window,
                cli,
                title: 'Select Provider CLI Binary',
                selectFolder: false,
                showToast,
                onPathSelected: binPath => binaryOverrideEntry.set_text(binPath),
            });
        });
        binaryOverrideEntry.add_suffix(pickBinBtn);

        const pollModeCombo = new Adw.ComboRow({
            title: escapeMarkup('Polling Mode'),
            subtitle: escapeMarkup('Active background polling or passive local cache updates only'),
            model: Gtk.StringList.new(POLL_MODE_OPTIONS.map(o => o.label)),
        });

        const switchCmdEntry = new Adw.EntryRow({
            title: escapeMarkup('External Switch Command (Optional)'),
        });

        providerCombo.connect('notify::selected', () => {
            if (cli.isDestroyed)
                return;
            const chosen = CANONICAL_PROVIDERS[providerCombo.get_selected()] ?? CANONICAL_PROVIDERS[0];
            providerCombo.set_subtitle(escapeMarkup(`Example location: ${chosen.locationHint}`));
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
            const pIdx = CANONICAL_PROVIDERS.findIndex(p => p.id === acc.provider);
            providerCombo.set_selected(pIdx >= 0 ? pIdx : 0);
            labelEntry.set_text(acc.label || '');
            idEntry.set_text(acc.id || '');
            pathEntry.set_text(acc.path || '');
            binaryOverrideEntry.set_text(acc.binary || '');
            pollModeCombo.set_selected(acc.poll_mode === 'passive' ? 1 : 0);
            switchCmdEntry.set_text(acc.switch_command || '');
            showToast(`Loaded "${acc.label || acc.id}" into editor below`);
        };

        const submitActionRow = new Adw.ActionRow({
            title: escapeMarkup('Save to config.toml'),
            subtitle: escapeMarkup('Writes [[providers.<id>.accounts]] safely and refreshes cached state'),
        });

        const clearFormBtn = new Gtk.Button({
            label: 'Clear',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        clearFormBtn.connect('clicked', () => resetForm());

        const saveBtn = new Gtk.Button({
            label: 'Save Account',
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
                    title: escapeMarkup('No manual accounts configured'),
                    subtitle: escapeMarkup('Auto-discovery handles default locations. Use the form below to add custom paths.'),
                });
                listGroup.add(emptyRow);
                dynamicRows.push(emptyRow);
                return;
            }

            for (const acc of accounts) {
                const provMeta = PROVIDER_BY_ID[acc.provider];
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
                    subtitle: escapeMarkup(details.join('  •  ') || 'Default path'),
                });

                const editBtn = new Gtk.Button({
                    icon_name: 'document-edit-symbolic',
                    tooltip_text: 'Edit manual account',
                    valign: Gtk.Align.CENTER,
                    css_classes: ['flat'],
                });
                editBtn.connect('clicked', () => populateFormForEdit(acc));

                const removeBtn = new Gtk.Button({
                    icon_name: 'user-trash-symbolic',
                    tooltip_text: 'Remove manual account',
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
                                showToast(`Removed manual account "${acc.label || acc.id}"`);
                            }
                        } catch (e) {
                            if (!cli.isDestroyed) {
                                removeBtn.set_sensitive(true);
                                showToast(`Failed to remove account: ${e.message}`);
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
                title: escapeMarkup('Could not query gnome-agent-status config'),
                subtitle: escapeMarkup(
                    message || 'Verify gnome-agent-status is installed or set Binary Path on Refresh & Visibility.'
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
            const chosenProv = CANONICAL_PROVIDERS[providerCombo.get_selected()] ?? CANONICAL_PROVIDERS[0];
            const rawLabel = labelEntry.get_text().trim();
            const rawId = idEntry.get_text().trim();
            const rawPath = pathEntry.get_text().trim();
            const rawBin = binaryOverrideEntry.get_text().trim();
            const pollOpt = POLL_MODE_OPTIONS[pollModeCombo.get_selected()] ?? POLL_MODE_OPTIONS[0];
            const rawSwitch = switchCmdEntry.get_text().trim();

            if (!rawLabel && !rawId && !rawPath) {
                showToast('Enter at least a label or custom path before saving');
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
                        showToast(`Saved manual account "${finalLabel}"`);
                    }
                } catch (e) {
                    if (!cli.isDestroyed)
                        showToast(`Failed to save manual account: ${e.message}`);
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
