import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {
    connectSettingChangedSafe,
    containPopoverEvents,
    createDesktopInterfaceSettings,
    createVBox,
    crossfadePages,
    getSettingStringSafe,
    getSettingStrvSafe,
} from '../compat.js';
import {
    DEFAULT_HIDDEN_PROVIDERS,
    DEFAULT_PROVIDER_ORDER,
    THEME_BY_ID,
    THEME_PRESETS,
    filterVisibleProviders,
    normalizeStatusReport,
    resolveEffectivePopoverLayout,
    sortProvidersByOrder,
} from '../format.js';
import {ProviderDetailPage} from './detailPage.js';
import {MainUsagePage} from './mainPage.js';
import {QuickOptionsPage} from './optionsPage.js';
import {ThemesPage} from './themesPage.js';
import {PopoverTooltipManager} from './widgets.js';

const COUNTDOWN_TICK_SECONDS = 60;

/**
 * Orchestrates the 4-page (`main`, `detail`, `options`, `themes`) popover
 * (`352px` compact list or `688px` wide 2-column Bento grid), 24-theme & font
 * class switching, `130ms` opacity crossfades (`0ms` reduced-motion), keyboard
 * `Escape`/`BackSpace`/arrow navigation, and an open-only 60s countdown timer.
 */
export class PopoverController {
    /**
     * @param {{
     *   indicator: import('../indicator.js').AgentStatusIndicator,
     *   settings: Gio.Settings,
     *   backend: import('../backend.js').BackendClient
     * }} params
     */
    constructor({indicator, settings, backend}) {
        this._indicator = indicator;
        this._settings = settings;
        this._backend = backend;
        this._desktopSettings = createDesktopInterfaceSettings();

        this._destroyed = false;
        this._menu = null;
        this._activePageName = 'main';
        this._selectedProviderId = null;
        this._isLightSurface = false;
        this._isWideGrid = false;

        this._rawReport = null;
        this._report = null;
        this._errorMessage = null;
        this._isLoading = false;

        this._countdownTimerId = 0;
        this._settingsSigIds = [];
        this._desktopSigIds = [];
        this._menuSigIds = [];
        this._menuActorSigIds = [];
        this._rootSigIds = [];
        this._rootDisposed = false;
        this._menuActorDisposed = false;
        this._themeContext = null;
        this._themeContextSigId = 0;

        this._rootBox = createVBox({
            style_class: 'gas-popover theme-midnight-slate',
            x_expand: true,
        });
        this._rootSigIds.push(...containPopoverEvents(this._rootBox));
        this._rootSigIds.push(
            this._rootBox.connect('key-press-event', (_actor, event) => this._onKeyPress(event)),
            this._rootBox.connect('destroy', () => {
                this._rootDisposed = true;
                this.destroy();
            })
        );

        const isMenuOpen = () => Boolean(this._menu?.isOpen);
        const getIcon = name => this._indicator.getIcon(name);

        this._tooltips = new PopoverTooltipManager({
            desktopSettings: this._desktopSettings,
            isMenuOpen,
        });

        this._mainPage = new MainUsagePage({
            settings: this._settings,
            getIcon,
            isMenuOpen,
            tooltips: this._tooltips,
            onRefresh: () => {
                void this._backend.collect(true);
            },
            onCopySummary: () => {
                this._indicator.copyUsageSummaryToClipboard();
            },
            onOpenOptions: () => {
                this.navigateTo('options');
            },
            onSelectProvider: providerId => {
                this._selectedProviderId = providerId;
                this.navigateTo('detail');
            },
            onOpenPreferences: () => {
                this._menu?.close();
                this._indicator.openExtensionPreferences();
            },
            onOpenConfig: () => {
                this._menu?.close();
                void this._indicator.openConfigFile();
            },
        });

        this._detailPage = new ProviderDetailPage({
            getIcon,
            isMenuOpen,
            tooltips: this._tooltips,
            onBack: () => {
                this.navigateTo('main');
            },
            onSelectAccount: (providerId, accountId) => {
                void this._backend.selectAccount(providerId, accountId);
            },
            onSignIn: loginCommand => {
                this._menu?.close();
                void this._backend.launchLoginTerminal(loginCommand);
            },
            onOpenDashboard: url => {
                this._menu?.close();
                try {
                    const ctx = global.create_app_launch_context?.(0, -1) ?? null;
                    Gio.AppInfo.launch_default_for_uri(url, ctx);
                } catch (e) {
                    console.error(`[gnome-agent-status] Failed to open dashboard URL: ${e.message}`);
                }
            },
            onOpenPreferences: () => {
                this._menu?.close();
                this._indicator.openExtensionPreferences();
            },
        });

        this._optionsPage = new QuickOptionsPage({
            settings: this._settings,
            tooltips: this._tooltips,
            onBack: () => {
                this.navigateTo('main');
            },
            onOpenThemes: () => {
                this.navigateTo('themes');
            },
            onLayoutChanged: () => {
                this._syncRootClasses();
                if (this._menu?.isOpen) {
                    this._refreshViews();
                    this._repaintActivePageBars();
                }
            },
            onOpenPreferences: () => {
                this._menu?.close();
                this._indicator.openExtensionPreferences();
            },
        });

        this._themesPage = new ThemesPage({
            settings: this._settings,
            tooltips: this._tooltips,
            onBack: () => {
                this.navigateTo('options');
            },
        });

        this._rootBox.add_child(this._mainPage.actor);
        this._rootBox.add_child(this._detailPage.actor);
        this._rootBox.add_child(this._optionsPage.actor);
        this._rootBox.add_child(this._themesPage.actor);

        this._connectSettings();
        this._syncRootClasses();
    }

    mount(menu) {
        this._menu = menu;
        this._menu.box.add_style_class_name('gas-popup-menu-box');
        this._menu.box.add_child(this._rootBox);

        this._menuSigIds.push(
            this._menu.connect('open-state-changed', (_m, isOpen) => {
                if (this._destroyed)
                    return;
                if (isOpen)
                    this._onMenuOpened();
                else
                    this._onMenuClosed();
            })
        );

        if (this._menu.actor) {
            this._menuActorSigIds.push(
                this._menu.actor.connect('captured-event', (_actor, event) => {
                    if (this._destroyed || event.type() !== Clutter.EventType.KEY_PRESS)
                        return Clutter.EVENT_PROPAGATE;
                    return this._onKeyPress(event);
                }),
                this._menu.actor.connect('destroy', () => {
                    this._menuActorDisposed = true;
                    this.destroy();
                })
            );
        }

        try {
            this._themeContext = St.ThemeContext.get_for_stage(global.stage);
            if (this._themeContext) {
                this._themeContextSigId = this._themeContext.connect('changed', () => {
                    if (!this._destroyed) {
                        this._syncRootClasses();
                        if (this._menu?.isOpen) {
                            this._refreshViews();
                            this._repaintActivePageBars();
                        }
                    }
                });
            }
        } catch (_e) {
            this._themeContext = null;
            this._themeContextSigId = 0;
        }

        this._syncRootClasses();
    }

    _connectSettings() {
        const watchedKeys = [
            'panel-mode',
            'auto-hide-when-empty',
            'density-mode',
            'theme',
            'font-style',
            'metric-mode',
            'hidden-providers',
        ];
        for (const key of watchedKeys) {
            this._settingsSigIds.push(
                this._settings.connect(`changed::${key}`, () => {
                    if (this._destroyed)
                        return;
                    this._syncRootClasses();
                    this._optionsPage.syncFromSettings();
                    this._themesPage.syncFromSettings();
                    if (this._menu?.isOpen) {
                        this._refreshViews();
                        this._repaintActivePageBars();
                    }
                })
            );
        }

        for (const optKey of ['provider-order', 'popover-layout']) {
            const sigId = connectSettingChangedSafe(
                this._settings,
                optKey,
                () => {
                    if (this._destroyed)
                        return;
                    this._syncRootClasses();
                    this._optionsPage.syncFromSettings();
                    if (this._menu?.isOpen) {
                        this._refreshViews();
                        this._repaintActivePageBars();
                    }
                }
            );
            if (sigId)
                this._settingsSigIds.push(sigId);
        }

        if (this._desktopSettings) {
            for (const dKey of ['color-scheme', 'gtk-theme']) {
                try {
                    this._desktopSigIds.push(
                        this._desktopSettings.connect(`changed::${dKey}`, () => {
                            if (this._destroyed)
                                return;
                            this._syncRootClasses();
                            if (this._menu?.isOpen) {
                                this._refreshViews();
                                this._repaintActivePageBars();
                            }
                        })
                    );
                } catch (_e) {
                    // Ignore missing desktop schema keys
                }
            }
        }
    }

    _getThemeId() {
        const raw = this._settings.get_string('theme') || 'midnight-slate';
        return THEME_BY_ID[raw] ? raw : 'midnight-slate';
    }

    _getEffectiveCairoThemeId() {
        const themeId = this._getThemeId();
        if (themeId === 'system' && this._isLightSurface)
            return 'system-light';
        return themeId;
    }

    _isLightHostSurface() {
        try {
            const scheme = this._desktopSettings?.get_string('color-scheme') || '';
            if (scheme === 'prefer-light')
                return true;
            if (scheme === 'prefer-dark')
                return false;
        } catch (_e) {
            // Continue to gtk-theme / host menu inspection
        }

        try {
            const gtkTheme = (this._desktopSettings?.get_string('gtk-theme') || '').toLowerCase();
            if (gtkTheme.endsWith('-dark') || gtkTheme.includes(':dark'))
                return false;
            if (gtkTheme.endsWith('-light') || gtkTheme.includes(':light'))
                return true;
        } catch (_e) {
            // Continue to unstripped context menu inspection
        }

        try {
            const refBox = this._indicator?._contextMenu?.box;
            if (refBox?.get_stage?.()) {
                const bg = refBox.get_theme_node().get_background_color();
                if ((bg.alpha ?? 0) >= 30) {
                    const toLinear = c => {
                        const v = c / 255.0;
                        return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
                    };
                    const lum =
                        0.2126 * toLinear(bg.red) +
                        0.7152 * toLinear(bg.green) +
                        0.0722 * toLinear(bg.blue);
                    return lum > 0.45;
                }
            }
        } catch (_e) {
            // Default to dark Shell surface
        }

        return false;
    }

    _getMonitorWidth() {
        try {
            const mon = Main.layoutManager?.findMonitorForActor?.(this._indicator) ??
                Main.layoutManager?.primaryMonitor ??
                null;
            if (mon && typeof mon.width === 'number' && mon.width > 0)
                return mon.width;
        } catch (_e) {}
        return 1920;
    }

    _applyWideGridState(visibleCount) {
        const layoutSetting = getSettingStringSafe(this._settings, 'popover-layout', 'list');
        const effective = resolveEffectivePopoverLayout(
            layoutSetting,
            visibleCount,
            this._getMonitorWidth()
        );
        const allowWideOnPage =
            this._activePageName === 'main' || this._activePageName === 'themes';
        const isWide = allowWideOnPage && effective === 'grid';
        this._isWideGrid = isWide;

        if (isWide)
            this._rootBox.add_style_class_name('is-wide-grid');
        else
            this._rootBox.remove_style_class_name('is-wide-grid');

        this._themesPage?.setWideGrid(isWide);
        return isWide;
    }

    _syncRootClasses() {
        const themeId = this._getThemeId();
        for (const preset of THEME_PRESETS) {
            const cls = `theme-${preset.id}`;
            if (preset.id === themeId)
                this._rootBox.add_style_class_name(cls);
            else
                this._rootBox.remove_style_class_name(cls);
        }

        const isMono = this._settings.get_string('font-style') === 'monospace';
        if (isMono)
            this._rootBox.add_style_class_name('font-monospace');
        else
            this._rootBox.remove_style_class_name('font-monospace');

        const isLightSurface = themeId === 'system' && this._isLightHostSurface();
        this._isLightSurface = isLightSurface;
        if (isLightSurface)
            this._rootBox.add_style_class_name('is-light-surface');
        else
            this._rootBox.remove_style_class_name('is-light-surface');

        this._tooltips?.setTheme({
            themeId,
            isLightSurface,
            isMonospace: isMono,
        });
    }

    _onKeyPress(event) {
        const symbol = event.get_key_symbol();

        if (symbol === Clutter.KEY_Down || symbol === Clutter.KEY_Up) {
            if (this._activePageName === 'main')
                return this._mainPage.handleKeyPress(event);
            if (this._activePageName === 'themes')
                return this._themesPage.handleKeyPress(event);
            if (this._activePageName === 'detail')
                return this._detailPage.handleKeyPress(event);
            return Clutter.EVENT_PROPAGATE;
        }

        if (this._activePageName === 'main')
            return Clutter.EVENT_PROPAGATE;

        if (
            symbol === Clutter.KEY_BackSpace ||
            symbol === Clutter.KEY_Escape ||
            (symbol === Clutter.KEY_Left && (event.get_state() & Clutter.ModifierType.MOD1_MASK))
        ) {
            if (this._activePageName === 'themes')
                this.navigateTo('options');
            else
                this.navigateTo('main');
            return Clutter.EVENT_STOP;
        }
        return Clutter.EVENT_PROPAGATE;
    }

    _getPageActor(pageName) {
        if (pageName === 'detail')
            return this._detailPage.actor;
        if (pageName === 'options')
            return this._optionsPage.actor;
        if (pageName === 'themes')
            return this._themesPage.actor;
        return this._mainPage.actor;
    }

    _restorePageFocus(pageName) {
        if (!this._menu?.isOpen)
            return;
        if (pageName === 'detail')
            this._detailPage.focusBackButton();
        else if (pageName === 'options')
            this._optionsPage.focusBackButton();
        else if (pageName === 'themes')
            this._themesPage.focusBackButton();
        else
            this._mainPage._optionsBtn?.grab_key_focus?.();
    }

    navigateTo(pageName, {animate = true} = {}) {
        if (this._destroyed || pageName === this._activePageName)
            return;

        this._tooltips?.hide();

        const currentFocus = global.stage?.get_key_focus?.();
        const hadFocusInside = Boolean(currentFocus && this._rootBox.contains(currentFocus));

        const outgoing = this._getPageActor(this._activePageName);
        const incoming = this._getPageActor(pageName);
        this._activePageName = pageName;

        this._refreshViews();

        if (!animate || !this._menu?.isOpen) {
            outgoing.hide();
            incoming.opacity = 255;
            incoming.show();
            if (hadFocusInside)
                this._restorePageFocus(pageName);
            this._repaintActivePageBars();
            return;
        }

        crossfadePages(outgoing, incoming, this._desktopSettings, () => {
            if (!this._destroyed) {
                if (hadFocusInside)
                    this._restorePageFocus(pageName);
                this._repaintActivePageBars();
            }
        });
        if (hadFocusInside)
            this._restorePageFocus(pageName);
        this._repaintActivePageBars();
    }

    setReport(report) {
        if (this._destroyed)
            return;
        this._rawReport = report;
        this._errorMessage = null;
        if (this._menu?.isOpen)
            this._refreshViews();
    }

    setError(message) {
        if (this._destroyed)
            return;
        this._errorMessage = message;
        if (this._menu?.isOpen)
            this._refreshViews();
    }

    setLoading(isLoading) {
        if (this._destroyed)
            return;
        this._isLoading = Boolean(isLoading);
        this._mainPage.setLoading(this._isLoading);
    }

    _onMenuOpened() {
        this._mainPage.resetDisclosureOnOpen();
        this._syncRootClasses();
        this._refreshViews();
        this._repaintActivePageBars();
        this._startCountdownTicker();

        const nowEpoch = Math.floor(Date.now() / 1000);
        const pollSec = Math.max(30, this._settings.get_int('refresh-interval-seconds') || 300);
        const age = this._report ? nowEpoch - (this._report.updated_at_epoch || 0) : Infinity;
        if (age >= pollSec)
            void this._backend.collect(false);
    }

    _onMenuClosed() {
        this._tooltips?.hide();
        this._stopCountdownTicker();
        if (this._activePageName !== 'main')
            this.navigateTo('main', {animate: false});
    }

    _startCountdownTicker() {
        this._stopCountdownTicker();
        this._countdownTimerId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT,
            COUNTDOWN_TICK_SECONDS,
            () => {
                if (this._destroyed || !this._menu?.isOpen) {
                    this._countdownTimerId = 0;
                    return GLib.SOURCE_REMOVE;
                }
                this._refreshViews();
                return GLib.SOURCE_CONTINUE;
            }
        );
        GLib.Source.set_name_by_id(this._countdownTimerId, '[gnome-agent-status] popover-countdown-tick');
    }

    _stopCountdownTicker() {
        if (this._countdownTimerId !== 0) {
            GLib.Source.remove(this._countdownTimerId);
            this._countdownTimerId = 0;
        }
    }

    _repaintActivePageBars() {
        if (!this._menu?.isOpen)
            return;
        if (this._activePageName === 'main')
            this._mainPage.repaintBars();
        else if (this._activePageName === 'detail')
            this._detailPage.repaintBars();
    }

    _refreshViews() {
        if (this._destroyed)
            return;

        const nowEpoch = Math.floor(Date.now() / 1000);
        this._report = this._rawReport ? normalizeStatusReport(this._rawReport, nowEpoch) : null;

        const densityMode = this._settings.get_string('density-mode') === 'compact'
            ? 'compact'
            : 'detailed';
        const metricMode = this._settings.get_string('metric-mode') === 'remaining'
            ? 'remaining'
            : 'used';
        const cairoThemeId = this._getEffectiveCairoThemeId();
        const hiddenIds = getSettingStrvSafe(
            this._settings,
            'hidden-providers',
            DEFAULT_HIDDEN_PROVIDERS
        );
        const orderIds = getSettingStrvSafe(
            this._settings,
            'provider-order',
            DEFAULT_PROVIDER_ORDER
        );
        const allProviders = this._report?.providers ?? [];
        const visibleProviders = sortProvidersByOrder(
            filterVisibleProviders(allProviders, hiddenIds),
            orderIds
        );

        const isWideGrid = this._applyWideGridState(visibleProviders.length);

        this._mainPage.update({
            visibleProviders,
            errorMessage: this._errorMessage,
            densityMode,
            metricMode,
            themeId: cairoThemeId,
            isWideGrid,
            nowEpoch,
        });

        if (this._activePageName === 'detail') {
            const target = allProviders.find(p => p.id === this._selectedProviderId);
            if (target) {
                this._detailPage.update({
                    provider: target,
                    reportUpdatedEpoch: this._report?.updated_at_epoch || 0,
                    metricMode,
                    themeId: cairoThemeId,
                    nowEpoch,
                });
            } else if (this._report) {
                this.navigateTo('main', {animate: false});
            }
        }
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;

        this._stopCountdownTicker();

        for (const sigId of this._settingsSigIds)
            this._settings.disconnect(sigId);
        this._settingsSigIds = [];

        if (this._desktopSettings) {
            for (const sigId of this._desktopSigIds)
                this._desktopSettings.disconnect(sigId);
        }
        this._desktopSigIds = [];

        if (this._menu) {
            for (const sigId of this._menuSigIds) {
                try {
                    this._menu.disconnect(sigId);
                } catch (_e) {}
            }
            if (this._menu.actor && !this._menuActorDisposed) {
                for (const sigId of this._menuActorSigIds) {
                    try {
                        this._menu.actor.disconnect(sigId);
                    } catch (_e) {}
                }
            }
        }
        this._menuSigIds = [];
        this._menuActorSigIds = [];

        if (this._themeContext && this._themeContextSigId) {
            try {
                this._themeContext.disconnect(this._themeContextSigId);
            } catch (_e) {}
        }
        this._themeContextSigId = 0;
        this._themeContext = null;

        if (this._rootBox && !this._rootDisposed) {
            for (const sigId of this._rootSigIds) {
                try {
                    this._rootBox.disconnect(sigId);
                } catch (_e) {}
            }
        }
        this._rootSigIds = [];

        if (!this._rootDisposed) {
            this._mainPage?.actor?.remove_all_transitions?.();
            this._detailPage?.actor?.remove_all_transitions?.();
            this._optionsPage?.actor?.remove_all_transitions?.();
            this._themesPage?.actor?.remove_all_transitions?.();
            this._mainPage.destroy();
            this._detailPage.destroy();
            this._themesPage.destroy();
        }
        this._tooltips?.destroy();
        this._tooltips = null;
        this._desktopSettings = null;
    }
}
