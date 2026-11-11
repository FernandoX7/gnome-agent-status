import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import {
    connectSettingChangedSafe,
    containPopoverEvents,
    createDesktopInterfaceSettings,
    createVBox,
    crossfadePages,
    getSettingStrvSafe,
} from '../compat.js';
import {
    DEFAULT_PROVIDER_ORDER,
    THEME_BY_ID,
    THEME_PRESETS,
    filterVisibleProviders,
    normalizeStatusReport,
    sortProvidersByOrder,
} from '../format.js';
import {ProviderDetailPage} from './detailPage.js';
import {MainUsagePage} from './mainPage.js';
import {QuickOptionsPage} from './optionsPage.js';
import {PopoverTooltipManager} from './widgets.js';

const COUNTDOWN_TICK_SECONDS = 60;

/**
 * Orchestrates the 3-page (`main`, `detail`, `options`) fixed-width (`352px`) popover,
 * theme & font class switching, `130ms` opacity crossfades (or `0ms` reduced-motion),
 * keyboard Escape/Back navigation, and an open-only 60s countdown timer.
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
            onOpenPreferences: () => {
                this._menu?.close();
                this._indicator.openExtensionPreferences();
            },
        });

        this._rootBox.add_child(this._mainPage.actor);
        this._rootBox.add_child(this._detailPage.actor);
        this._rootBox.add_child(this._optionsPage.actor);

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
                        if (this._menu?.isOpen)
                            this._refreshViews();
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
                    if (this._menu?.isOpen)
                        this._refreshViews();
                })
            );
        }

        const orderSigId = connectSettingChangedSafe(
            this._settings,
            'provider-order',
            () => {
                if (this._destroyed)
                    return;
                if (this._menu?.isOpen)
                    this._refreshViews();
            }
        );
        if (orderSigId)
            this._settingsSigIds.push(orderSigId);

        if (this._desktopSettings) {
            for (const dKey of ['color-scheme', 'gtk-theme']) {
                try {
                    this._desktopSigIds.push(
                        this._desktopSettings.connect(`changed::${dKey}`, () => {
                            if (this._destroyed)
                                return;
                            this._syncRootClasses();
                            if (this._menu?.isOpen)
                                this._refreshViews();
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

    _isLightHostSurface() {
        try {
            if (this._menu?.box?.get_stage?.() && this._menu.box.mapped) {
                const bg = this._menu.box.get_theme_node().get_background_color();
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
            // Fall through to desktop color-scheme preference
        }

        try {
            return this._desktopSettings?.get_string('color-scheme') === 'prefer-light';
        } catch (_e) {
            return false;
        }
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

        if (this._menu?.box) {
            for (const preset of THEME_PRESETS) {
                const cls = `gas-host-${preset.id}`;
                if (preset.id === themeId)
                    this._menu.box.add_style_class_name(cls);
                else
                    this._menu.box.remove_style_class_name(cls);
            }
        }

        const isLightSurface = themeId === 'system' && this._isLightHostSurface();
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
        if (this._activePageName === 'main')
            return Clutter.EVENT_PROPAGATE;

        const symbol = event.get_key_symbol();
        if (
            symbol === Clutter.KEY_BackSpace ||
            symbol === Clutter.KEY_Escape ||
            (symbol === Clutter.KEY_Left && (event.get_state() & Clutter.ModifierType.MOD1_MASK))
        ) {
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
        return this._mainPage.actor;
    }

    _restorePageFocus(pageName) {
        if (!this._menu?.isOpen)
            return;
        if (pageName === 'detail')
            this._detailPage.focusBackButton();
        else if (pageName === 'options')
            this._optionsPage.focusBackButton();
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
        const themeId = this._getThemeId();
        const hiddenIds = this._settings.get_strv('hidden-providers') ?? [];
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

        this._mainPage.update({
            visibleProviders,
            errorMessage: this._errorMessage,
            densityMode,
            metricMode,
            themeId,
            nowEpoch,
        });

        if (this._activePageName === 'detail') {
            const target = allProviders.find(p => p.id === this._selectedProviderId);
            if (target) {
                this._detailPage.update({
                    provider: target,
                    reportUpdatedEpoch: this._report?.updated_at_epoch || 0,
                    metricMode,
                    themeId,
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
            this._mainPage.destroy();
            this._detailPage.destroy();
        }
        this._tooltips?.destroy();
        this._tooltips = null;
        this._desktopSettings = null;
    }
}
