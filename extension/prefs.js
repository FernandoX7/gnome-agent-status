import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/shell/extensions/prefs.js';

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

const POLL_MODE_OPTIONS = Object.freeze([
    Object.freeze({id: 'active', label: 'Active Polling'}),
    Object.freeze({id: 'passive', label: 'Passive Cache Only'}),
]);

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
 * Helper binding a string GSetting to an `Adw.ComboRow` over `{id, label}` choices.
 *
 * @param {{
 *   title: string,
 *   subtitle?: string,
 *   options: ReadonlyArray<{id: string, label: string}>,
 *   settings: Gio.Settings,
 *   key: string
 * }} params
 * @returns {Adw.ComboRow}
 */
function createBoundComboRow({title, subtitle, options, settings, key}) {
    const stringList = Gtk.StringList.new(options.map(o => o.label));
    const row = new Adw.ComboRow({
        title,
        subtitle: subtitle || '',
        model: stringList,
    });

    let updating = false;
    const syncFromSettings = () => {
        const val = settings.get_string(key);
        const idx = options.findIndex(o => o.id === val);
        updating = true;
        row.set_selected(idx >= 0 ? idx : 0);
        updating = false;
    };

    syncFromSettings();

    row.connect('notify::selected', () => {
        if (updating)
            return;
        const chosen = options[row.get_selected()];
        if (chosen && settings.get_string(key) !== chosen.id)
            settings.set_string(key, chosen.id);
    });

    const sigId = settings.connect(`changed::${key}`, () => {
        syncFromSettings();
    });
    row.connect('destroy', () => {
        settings.disconnect(sigId);
    });

    return row;
}

/**
 * Async CLI runner scoped to the lifetime of the Preferences window.
 */
class PrefsCliRunner {
    constructor(settings) {
        this._settings = settings;
        this._destroyed = false;
        this._resolvedBin = null;
        this._activeProcs = new Set();
        this._cancellables = new Set();

        this._binSigId = this._settings.connect('changed::binary-path', () => {
            this._resolvedBin = null;
        });
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;

        if (this._binSigId) {
            this._settings.disconnect(this._binSigId);
            this._binSigId = 0;
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
        this._cancellables.add(c);
        return c;
    }

    releaseCancellable(c) {
        this._cancellables.delete(c);
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
            const [stdout, stderr] = await new Promise((resolve, reject) => {
                proc.communicate_utf8_async(null, cancellable, (src, res) => {
                    try {
                        const [, out, err] = src.communicate_utf8_finish(res);
                        resolve([out ?? '', err ?? '']);
                    } catch (err) {
                        reject(err);
                    }
                });
            });

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

    triggerBackgroundCollect() {
        if (this._destroyed)
            return;
        void this.runJsonAsync(['collect']).catch(() => {
            // Non-fatal background cache refresh after config mutation
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
            if (!message)
                return;
            try {
                window.add_toast(new Adw.Toast({title: message}));
            } catch (_e) {
                // Ignore if window is closing
            }
        };

        window.add(this._buildAppearancePage(settings));
        const refreshHooks = this._buildRefreshVisibilityPage(window, settings, cli, showToast);
        window.add(refreshHooks.page);
        const accountsHooks = this._buildAccountsPage(window, cli, showToast);
        window.add(accountsHooks.page);

        // Load initial `gnome-agent-status config show` asynchronously
        void (async () => {
            try {
                const cfg = await cli.runJsonAsync(['config', 'show']);
                if (!cfg)
                    return;
                refreshHooks.applyConfigState(cfg);
                accountsHooks.applyConfigState(cfg);
            } catch (e) {
                accountsHooks.showLoadError(e.message);
            }
        })();
    }

    _buildAppearancePage(settings) {
        const page = new Adw.PreferencesPage({
            title: 'Appearance & Top Bar',
            icon_name: 'preferences-desktop-appearance-symbolic',
        });

        // Top Bar Indicator Group
        const panelGroup = new Adw.PreferencesGroup({
            title: 'Top Bar Indicator',
            description: 'Configure how AI agent usage appears in the GNOME top panel.',
        });

        panelGroup.add(createBoundComboRow({
            title: 'Panel Mode',
            subtitle: 'Minimal single icon, compact provider pills, or full text summary',
            options: PANEL_MODES,
            settings,
            key: 'panel-mode',
        }));

        panelGroup.add(createBoundComboRow({
            title: 'Panel Box Alignment',
            subtitle: 'Section of the GNOME top bar where the indicator sits',
            options: PANEL_BOXES,
            settings,
            key: 'panel-box',
        }));

        const posRow = new Adw.SpinRow({
            title: 'Panel Position Index',
            subtitle: 'Zero-based slot index within the chosen top bar box',
            adjustment: new Gtk.Adjustment({
                lower: 0,
                upper: 25,
                step_increment: 1,
                page_increment: 5,
                value: settings.get_int('panel-position'),
            }),
        });
        settings.bind('panel-position', posRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        panelGroup.add(posRow);

        const autoHideRow = new Adw.SwitchRow({
            title: 'Auto-Hide When Empty',
            subtitle: 'Hide the top bar icon automatically when no active providers are detected',
        });
        settings.bind('auto-hide-when-empty', autoHideRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        panelGroup.add(autoHideRow);

        page.add(panelGroup);

        // Popover Theme & Typography Group
        const themeGroup = new Adw.PreferencesGroup({
            title: 'Popover Theme & Layout',
            description: 'Customize colors, information density, and number formatting.',
        });

        themeGroup.add(createBoundComboRow({
            title: 'Theme Preset',
            subtitle: 'Applies immediately to the expandable usage popover and gauges',
            options: THEME_PRESETS,
            settings,
            key: 'theme',
        }));

        themeGroup.add(createBoundComboRow({
            title: 'Default Popover Density',
            subtitle: 'Detailed multi-window breakdown or compact single-line summary',
            options: DENSITY_MODES,
            settings,
            key: 'density-mode',
        }));

        themeGroup.add(createBoundComboRow({
            title: 'Font Style',
            subtitle: 'System UI font with tabular numbers or developer monospace font',
            options: FONT_STYLES,
            settings,
            key: 'font-style',
        }));

        themeGroup.add(createBoundComboRow({
            title: 'Quota Metric',
            subtitle: 'Display percentages and gauges as quota used or quota remaining',
            options: METRIC_MODES,
            settings,
            key: 'metric-mode',
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
            title: 'Collection Cadence & Binary',
            description: 'Control background polling intervals, discovery defaults, and binary path.',
        });

        const refreshSpin = new Adw.SpinRow({
            title: 'Background Refresh Interval (seconds)',
            subtitle: 'Minimum 300s recommended to stay well within provider OAuth rate limits',
            adjustment: new Gtk.Adjustment({
                lower: 60,
                upper: 3600,
                step_increment: 30,
                page_increment: 60,
                value: Math.max(60, settings.get_int('refresh-interval-seconds') || 300),
            }),
        });
        settings.bind('refresh-interval-seconds', refreshSpin, 'value', Gio.SettingsBindFlags.DEFAULT);
        cadenceGroup.add(refreshSpin);

        let syncingConfig = false;

        const autoDiscoverRow = new Adw.SwitchRow({
            title: 'Auto-Discover Local Accounts',
            subtitle: 'Automatically detect local Claude, Codex, Grok, Antigravity, and Cursor profiles',
            active: true,
        });
        autoDiscoverRow.connect('notify::active', () => {
            if (syncingConfig)
                return;
            const flag = autoDiscoverRow.get_active() ? 'true' : 'false';
            void (async () => {
                try {
                    await cli.runJsonAsync(['config', 'set', '--auto-discover', flag]);
                    cli.triggerBackgroundCollect();
                    showToast('Updated auto-discovery setting');
                } catch (e) {
                    showToast(`Failed to update config.toml: ${e.message}`);
                }
            })();
        });
        cadenceGroup.add(autoDiscoverRow);

        const timeoutSpin = new Adw.SpinRow({
            title: 'Collector Timeout (seconds)',
            subtitle: 'Maximum wait time per collection run in config.toml',
            adjustment: new Gtk.Adjustment({
                lower: 5,
                upper: 180,
                step_increment: 5,
                page_increment: 15,
                value: 30,
            }),
        });
        timeoutSpin.connect('notify::value', () => {
            if (syncingConfig)
                return;
            const secs = String(Math.round(timeoutSpin.get_value()));
            void (async () => {
                try {
                    await cli.runJsonAsync(['config', 'set', '--timeout-seconds', secs]);
                } catch (e) {
                    showToast(`Failed to update timeout: ${e.message}`);
                }
            })();
        });
        cadenceGroup.add(timeoutSpin);

        // Custom binary override with Gtk.FileDialog
        const binaryEntry = new Adw.EntryRow({
            title: 'Custom gnome-agent-status Binary Path (Optional)',
            text: settings.get_string('binary-path') || '',
        });
        binaryEntry.connect('changed', () => {
            const val = binaryEntry.get_text().trim();
            if (settings.get_string('binary-path') !== val)
                settings.set_string('binary-path', val);
        });

        const browseBinBtn = new Gtk.Button({
            label: 'Browse…',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        browseBinBtn.connect('clicked', () => {
            const dialog = new Gtk.FileDialog({
                title: 'Select gnome-agent-status Executable',
            });
            const cancellable = cli.newCancellable();
            dialog.open(window, cancellable, (src, res) => {
                cli.releaseCancellable(cancellable);
                try {
                    const file = src.open_finish(res);
                    const path = file?.get_path();
                    if (path)
                        binaryEntry.set_text(path);
                } catch (_e) {
                    // User dismissed dialog
                }
            });
        });
        binaryEntry.add_suffix(browseBinBtn);
        cadenceGroup.add(binaryEntry);

        page.add(cadenceGroup);

        // Provider Visibility Group
        const visibilityGroup = new Adw.PreferencesGroup({
            title: 'Visible Providers',
            description: 'Choose which providers appear in the top bar and usage popover.',
        });

        for (const prov of CANONICAL_PROVIDERS) {
            const hiddenInitial = new Set(settings.get_strv('hidden-providers') || []);
            const switchRow = new Adw.SwitchRow({
                title: prov.displayName,
                subtitle: `Provider ID: ${prov.id}`,
                active: !hiddenInitial.has(prov.id),
            });

            let internalSync = false;
            switchRow.connect('notify::active', () => {
                if (internalSync)
                    return;
                const currentHidden = new Set(settings.get_strv('hidden-providers') || []);
                if (switchRow.get_active())
                    currentHidden.delete(prov.id);
                else
                    currentHidden.add(prov.id);
                settings.set_strv('hidden-providers', Array.from(currentHidden));
            });

            const sigId = settings.connect('changed::hidden-providers', () => {
                const updatedHidden = new Set(settings.get_strv('hidden-providers') || []);
                const shouldBeActive = !updatedHidden.has(prov.id);
                if (switchRow.get_active() !== shouldBeActive) {
                    internalSync = true;
                    switchRow.set_active(shouldBeActive);
                    internalSync = false;
                }
            });
            switchRow.connect('destroy', () => settings.disconnect(sigId));

            visibilityGroup.add(switchRow);
        }

        page.add(visibilityGroup);

        return {
            page,
            applyConfigState: cfg => {
                syncingConfig = true;
                if (typeof cfg.auto_discover === 'boolean')
                    autoDiscoverRow.set_active(cfg.auto_discover);
                if (typeof cfg.timeout_seconds === 'number' && cfg.timeout_seconds >= 5)
                    timeoutSpin.set_value(cfg.timeout_seconds);
                syncingConfig = false;
            },
        };
    }

    _buildAccountsPage(window, cli, showToast) {
        const page = new Adw.PreferencesPage({
            title: 'Accounts & Manual Locations',
            icon_name: 'system-users-symbolic',
        });

        // 1. Configured Manual Accounts Group
        const listGroup = new Adw.PreferencesGroup({
            title: 'Configured Manual Account Locations',
            description: 'Manual directories, credential files, or databases stored in config.toml.',
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

        const openConfigBtn = new Gtk.Button({
            label: 'Open config.toml',
            css_classes: ['flat'],
        });
        openConfigBtn.connect('clicked', () => {
            void (async () => {
                try {
                    const cfgFile = Gio.File.new_for_path(currentConfigPath);
                    const exists = await new Promise(resolve => {
                        cfgFile.query_info_async(
                            'standard::type',
                            Gio.FileQueryInfoFlags.NONE,
                            GLib.PRIORITY_DEFAULT,
                            null,
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
                    if (!exists)
                        await cli.runRawAsync(['--init-config']).catch(() => {});
                    const uri = GLib.filename_to_uri(currentConfigPath, null);
                    Gio.AppInfo.launch_default_for_uri(uri, null);
                } catch (e) {
                    showToast(`Could not open config.toml: ${e.message}`);
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
            title: 'Add or Update Manual Account',
            description: 'Point manually to any custom Claude, Codex, Grok, Antigravity, or Cursor directory or file if auto-discovery does not find it.',
        });

        const providerCombo = new Adw.ComboRow({
            title: 'Provider',
            subtitle: `Example location: ${CANONICAL_PROVIDERS[0].locationHint}`,
            model: Gtk.StringList.new(CANONICAL_PROVIDERS.map(p => p.displayName)),
        });

        const labelEntry = new Adw.EntryRow({
            title: 'Display Label (e.g. Work Claude)',
        });

        const idEntry = new Adw.EntryRow({
            title: 'Account ID Slug (e.g. work — auto-filled if blank)',
        });

        const pathEntry = new Adw.EntryRow({
            title: 'Profile Directory or Auth/DB File Path',
        });

        const pickFolderBtn = new Gtk.Button({
            label: 'Folder…',
            tooltip_text: 'Select a custom profile directory (e.g. ~/.claude-work or ~/.codex-work)',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickFolderBtn.connect('clicked', () => {
            const dialog = new Gtk.FileDialog({
                title: 'Select Custom Account Directory',
            });
            const cancellable = cli.newCancellable();
            dialog.select_folder(window, cancellable, (src, res) => {
                cli.releaseCancellable(cancellable);
                try {
                    const folder = src.select_folder_finish(res);
                    const folderPath = folder?.get_path();
                    if (folderPath)
                        pathEntry.set_text(folderPath);
                } catch (_e) {
                    // Dismissed
                }
            });
        });

        const pickFileBtn = new Gtk.Button({
            label: 'File…',
            tooltip_text: 'Select a specific credential or database file (e.g. state.vscdb or auth.json)',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickFileBtn.connect('clicked', () => {
            const dialog = new Gtk.FileDialog({
                title: 'Select Account Credential or Database File',
            });
            const cancellable = cli.newCancellable();
            dialog.open(window, cancellable, (src, res) => {
                cli.releaseCancellable(cancellable);
                try {
                    const file = src.open_finish(res);
                    const filePath = file?.get_path();
                    if (filePath)
                        pathEntry.set_text(filePath);
                } catch (_e) {
                    // Dismissed
                }
            });
        });

        pathEntry.add_suffix(pickFolderBtn);
        pathEntry.add_suffix(pickFileBtn);

        const binaryOverrideEntry = new Adw.EntryRow({
            title: 'Custom Provider CLI Binary (Optional)',
        });
        const pickBinBtn = new Gtk.Button({
            label: 'Browse…',
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        pickBinBtn.connect('clicked', () => {
            const dialog = new Gtk.FileDialog({
                title: 'Select Provider CLI Binary',
            });
            const cancellable = cli.newCancellable();
            dialog.open(window, cancellable, (src, res) => {
                cli.releaseCancellable(cancellable);
                try {
                    const file = src.open_finish(res);
                    const binPath = file?.get_path();
                    if (binPath)
                        binaryOverrideEntry.set_text(binPath);
                } catch (_e) {
                    // Dismissed
                }
            });
        });
        binaryOverrideEntry.add_suffix(pickBinBtn);

        const pollModeCombo = new Adw.ComboRow({
            title: 'Polling Mode',
            subtitle: 'Active background polling or passive local cache updates only',
            model: Gtk.StringList.new(POLL_MODE_OPTIONS.map(o => o.label)),
        });

        const switchCmdEntry = new Adw.EntryRow({
            title: 'External Switch Command (Optional)',
        });

        providerCombo.connect('notify::selected', () => {
            const chosen = CANONICAL_PROVIDERS[providerCombo.get_selected()] ?? CANONICAL_PROVIDERS[0];
            providerCombo.set_subtitle(`Example location: ${chosen.locationHint}`);
        });

        const resetForm = () => {
            providerCombo.set_selected(0);
            labelEntry.set_text('');
            idEntry.set_text('');
            pathEntry.set_text('');
            binaryOverrideEntry.set_text('');
            pollModeCombo.set_selected(0);
            switchCmdEntry.set_text('');
        };

        const populateFormForEdit = acc => {
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
            title: 'Save to config.toml',
            subtitle: 'Writes [[providers.<id>.accounts]] safely and refreshes cached state',
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
            clearDynamicRows();
            if (cfg?.config_path)
                currentConfigPath = cfg.config_path;

            const accounts = Array.isArray(cfg?.manual_accounts) ? cfg.manual_accounts : [];
            if (accounts.length === 0) {
                const emptyRow = new Adw.ActionRow({
                    title: 'No manual accounts configured',
                    subtitle: 'Auto-discovery handles default locations. Use the form below to add custom paths.',
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
                    title: `${provName} · ${acc.label || acc.id} (${acc.id})`,
                    subtitle: details.join('  •  ') || 'Default path',
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
                    removeBtn.set_sensitive(false);
                    void (async () => {
                        try {
                            const updated = await cli.runJsonAsync([
                                'config',
                                'remove-account',
                                '--provider',
                                String(acc.provider),
                                '--id',
                                String(acc.id),
                            ]);
                            if (updated) {
                                renderManualAccounts(updated);
                                cli.triggerBackgroundCollect();
                                showToast(`Removed manual account "${acc.label || acc.id}"`);
                            }
                        } catch (e) {
                            removeBtn.set_sensitive(true);
                            showToast(`Failed to remove account: ${e.message}`);
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
            clearDynamicRows();
            const errRow = new Adw.ActionRow({
                title: 'Could not query gnome-agent-status config',
                subtitle: message || 'Verify gnome-agent-status is installed or set Binary Path on Refresh & Visibility.',
            });
            listGroup.add(errRow);
            dynamicRows.push(errRow);
        };

        reloadBtn.connect('clicked', () => {
            void (async () => {
                try {
                    const cfg = await cli.runJsonAsync(['config', 'show']);
                    if (cfg)
                        renderManualAccounts(cfg);
                } catch (e) {
                    showLoadError(e.message);
                }
            })();
        });

        saveBtn.connect('clicked', () => {
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

            saveBtn.set_sensitive(false);
            void (async () => {
                try {
                    const updated = await cli.runJsonAsync(args);
                    if (updated) {
                        renderManualAccounts(updated);
                        resetForm();
                        cli.triggerBackgroundCollect();
                        showToast(`Saved manual account "${finalLabel}"`);
                    }
                } catch (e) {
                    showToast(`Failed to save manual account: ${e.message}`);
                } finally {
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
