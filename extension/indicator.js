import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {createHBox} from './compat.js';
import {
    computePanelSummary,
    filterVisibleProviders,
} from './format.js';

export const AgentStatusIndicator = GObject.registerClass(
class AgentStatusIndicator extends PanelMenu.Button {
    _init({extension, settings, backend, buildFallbackMenu = false}) {
        super._init(0.5, 'Agent Usage Status', false);

        this._extension = extension;
        this._settings = settings;
        this._backend = backend;
        this._popoverController = null;

        this._destroyed = false;
        this._hasHydratedOnce = false;
        this._lastReport = null;
        this._lastError = null;
        this._isLoading = false;

        this._iconCache = new Map();
        this._pillPool = [];
        this._settingsSigIds = [];
        this._menuSigIds = [];
        this._menuActorSigIds = [];
        this._menuActorDisposed = false;
        this._contextMenuSigIds = [];
        this._contextMenuActorSigIds = [];
        this._contextMenuActorDisposed = false;
        this._selfSigIds = [];
        this._themeContext = null;
        this._themeContextSigId = 0;
        this._contextMenu = null;
        this._contextMenuManager = null;
        this._secondaryClickGesture = null;
        this._refreshMenuItem = null;
        this._settingsMenuItem = null;
        this._quitMenuItem = null;
        this._quitIdleId = 0;

        this._rootBox = createHBox({
            style_class: 'panel-status-menu-box gas-panel-box',
            y_align: Clutter.ActorAlign.CENTER,
        });

        // Mode 1: Minimal symbolic icon + subtle calm-tech semantic dot
        this._iconModeBox = createHBox({
            style_class: 'gas-panel-icon-wrap',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._symbolicIcon = new St.Icon({
            gicon: this.getIcon('agent-status-symbolic'),
            icon_size: 16,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'system-status-icon gas-panel-icon',
        });
        this._statusDot = new St.Widget({
            style_class: 'gas-panel-dot',
            y_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });
        this._iconModeBox.add_child(this._symbolicIcon);
        this._iconModeBox.add_child(this._statusDot);

        // Mode 2: Compact inline pills (mini icon + tabular %)
        this._pillsModeBox = createHBox({
            style_class: 'gas-panel-pills-box',
            y_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });
        this._pillsEmptyIcon = new St.Icon({
            gicon: this.getIcon('agent-status-symbolic'),
            icon_size: 15,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'system-status-icon gas-panel-icon',
        });
        this._pillsEmptyLabel = new St.Label({
            text: '—',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-panel-pill-label',
        });
        this._pillsEmptyWrap = createHBox({
            style_class: 'gas-panel-pill',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._pillsEmptyWrap.add_child(this._pillsEmptyIcon);
        this._pillsEmptyWrap.add_child(this._pillsEmptyLabel);
        this._pillsModeBox.add_child(this._pillsEmptyWrap);

        // Mode 3: Classic full-text bar string
        this._fullTextLabel = new St.Label({
            text: 'Agents: …',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-panel-full-text',
            visible: false,
        });

        this._rootBox.add_child(this._iconModeBox);
        this._rootBox.add_child(this._pillsModeBox);
        this._rootBox.add_child(this._fullTextLabel);
        this.add_child(this._rootBox);

        if (buildFallbackMenu)
            this._buildFallbackMenu();
        this._setupClickGestures();
        this._buildContextMenu();
        this._connectSignals();
        this._syncPanelPresentation();
    }

    /**
     * Loads and caches a bundled SVG icon from `extension/icons/<iconName>.svg`.
     *
     * @param {string} iconName
     * @returns {Gio.Icon}
     */
    getIcon(iconName) {
        const safeName = iconName || 'agent-status-symbolic';
        if (this._iconCache.has(safeName))
            return this._iconCache.get(safeName);

        const svgPath = GLib.build_filenamev([
            this._extension.path,
            'icons',
            `${safeName}.svg`,
        ]);
        const file = Gio.File.new_for_path(svgPath);
        const gicon = new Gio.FileIcon({file});
        this._iconCache.set(safeName, gicon);
        return gicon;
    }

    /**
     * Attaches the rich multi-page PopoverController, replacing the minimal fallback menu.
     *
     * @param {object} controller
     */
    attachPopoverController(controller) {
        this._fallbackItems = null;
        this.menu.removeAll();
        this._popoverController = controller;
        this._popoverController.mount(this.menu);
        if (this._lastReport)
            this._popoverController.setReport(this._lastReport);
        if (this._lastError)
            this._popoverController.setError(this._lastError);
        this._popoverController.setLoading(this._isLoading);
    }

    _buildFallbackMenu() {
        this.menu.removeAll();
        const statusItem = new PopupMenu.PopupMenuItem('Loading agent usage…', {
            reactive: false,
        });
        const refreshItem = new PopupMenu.PopupMenuItem('Refresh Now');
        refreshItem.connect('activate', () => {
            void this._backend.collect(true);
        });
        const configItem = new PopupMenu.PopupMenuItem('Open Config');
        configItem.connect('activate', () => {
            void this.openConfigFile();
        });
        this.menu.addMenuItem(statusItem);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addMenuItem(refreshItem);
        this.menu.addMenuItem(configItem);
        this._fallbackItems = {statusItem};
    }

    _setupClickGestures() {
        // GNOME Shell 49/50 PanelMenu.Button binds `this._clickGesture` with
        // `required_button = 0` (any button). Restrict it to primary left-click
        // and add a dedicated secondary right-click gesture for the context menu.
        if (this._clickGesture && typeof this._clickGesture.set_required_button === 'function')
            this._clickGesture.set_required_button(Clutter.BUTTON_PRIMARY);

        if (typeof Clutter.ClickGesture === 'function') {
            this._secondaryClickGesture = new Clutter.ClickGesture({
                required_button: Clutter.BUTTON_SECONDARY,
                recognize_on_press: true,
            });
            this._secondaryClickGesture.connect('recognize', () => {
                this._toggleContextMenu();
            });
            this.add_action(this._secondaryClickGesture);
        } else if (!this._clickGesture) {
            // GNOME Shell 45-48 legacy fallback where Clutter.ClickGesture does not exist.
            this._selfSigIds.push(
                this.connect('button-press-event', (_actor, event) => {
                    if (event.get_button() === Clutter.BUTTON_SECONDARY) {
                        this._toggleContextMenu();
                        return Clutter.EVENT_STOP;
                    }
                    return Clutter.EVENT_PROPAGATE;
                })
            );
        }
    }

    _buildContextMenu() {
        this._contextMenu = new PopupMenu.PopupMenu(this, 0.5, St.Side.TOP);
        this._contextMenu.actor.add_style_class_name('panel-menu');
        this._contextMenu.actor.add_style_class_name('gas-context-menu');
        Main.uiGroup.add_child(this._contextMenu.actor);
        this._contextMenu.actor.hide();

        this._refreshMenuItem = new PopupMenu.PopupImageMenuItem(
            'Refresh Usage',
            'view-refresh-symbolic'
        );
        this._refreshMenuItem.connect('activate', () => {
            if (!this._destroyed)
                void this._backend?.collect(true);
        });

        this._settingsMenuItem = new PopupMenu.PopupImageMenuItem(
            'Settings…',
            'preferences-system-symbolic'
        );
        this._settingsMenuItem.connect('activate', () => {
            if (!this._destroyed)
                this.openExtensionPreferences();
        });

        this._quitMenuItem = new PopupMenu.PopupImageMenuItem(
            'Quit Agent Status',
            'application-exit-symbolic'
        );
        this._quitMenuItem.connect('activate', () => {
            if (this._destroyed)
                return;
            const uuid = this._extension?.uuid;
            this._contextMenu?.close(false);
            this.menu?.close(false);
            if (!uuid || this._quitIdleId !== 0)
                return;
            this._quitIdleId = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
                this._quitIdleId = 0;
                try {
                    Main.extensionManager?.disableExtension?.(uuid);
                } catch (e) {
                    console.error(`[gnome-agent-status] Failed to disable extension: ${e.message}`);
                }
                return GLib.SOURCE_REMOVE;
            });
            GLib.Source.set_name_by_id?.(this._quitIdleId, '[gnome-agent-status] quit-extension');
        });

        this._contextMenu.addMenuItem(this._refreshMenuItem);
        this._contextMenu.addMenuItem(this._settingsMenuItem);
        this._contextMenu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._contextMenu.addMenuItem(this._quitMenuItem);
    }

    /**
     * Registers the secondary right-click context menu with `Main.panel.menuManager`
     * after `Main.panel.addToStatusArea` has registered `this.menu`, ensuring
     * panel hover-sliding prefers the primary usage popover while outside clicks
     * dismiss the context menu natively.
     *
     * @param {object} [menuManager]
     */
    registerContextMenu(menuManager = Main.panel?.menuManager) {
        if (this._destroyed || !this._contextMenu || !menuManager)
            return;
        if (this._contextMenuManager === menuManager)
            return;
        this._contextMenuManager = menuManager;
        menuManager.addMenu(this._contextMenu);
    }

    _openContextMenu(animate = true) {
        if (this._destroyed || !this._contextMenu)
            return;
        this.registerContextMenu();
        if (this.menu?.isOpen)
            this.menu.close(false);
        this._contextMenu.open(animate);
    }

    _toggleContextMenu() {
        if (this._destroyed || !this._contextMenu)
            return;
        this.registerContextMenu();
        if (this._contextMenu.isOpen) {
            this._contextMenu.close(true);
            return;
        }
        if (this.menu?.isOpen)
            this.menu.close(false);
        this._contextMenu.open(true);
    }

    _isEventOnIndicator(event, menuActor) {
        const targetActor = global.stage.get_event_actor?.(event) ?? null;
        if (menuActor && targetActor && menuActor.contains(targetActor))
            return false;
        if (targetActor && this.contains(targetActor))
            return true;
        if (!this.mapped)
            return false;
        const [ex, ey] = event.get_coords();
        const [ix, iy] = this.get_transformed_position();
        const [iw, ih] = this.get_transformed_size();
        return ex >= ix && ex <= ix + iw && ey >= iy && ey <= iy + ih;
    }

    vfunc_hide() {
        super.vfunc_hide();
        if (this._contextMenu?.isOpen)
            this._contextMenu.close(false);
    }

    _connectSignals() {
        const watchedKeys = [
            'panel-mode',
            'auto-hide-when-empty',
            'font-style',
            'metric-mode',
            'hidden-providers',
        ];
        for (const key of watchedKeys) {
            this._settingsSigIds.push(
                this._settings.connect(`changed::${key}`, () => {
                    if (!this._destroyed)
                        this._syncPanelPresentation();
                })
            );
        }

        this._menuSigIds.push(
            this.menu.connect('open-state-changed', (_menu, isOpen) => {
                if (this._destroyed)
                    return;
                if (isOpen) {
                    if (this._contextMenu?.isOpen)
                        this._contextMenu.close(false);
                } else {
                    if (this._contextMenu?.isOpen)
                        this.add_style_pseudo_class('active');
                    this._updateAutoHideVisibility();
                }
            })
        );

        if (this.menu?.actor) {
            this._menuActorSigIds.push(
                this.menu.actor.connect('captured-event', (actor, event) => {
                    if (
                        !this._destroyed &&
                        this.menu?.isOpen &&
                        event.type() === Clutter.EventType.BUTTON_PRESS &&
                        event.get_button() === Clutter.BUTTON_SECONDARY &&
                        this._isEventOnIndicator(event, actor)
                    ) {
                        this.menu.close(false);
                        this._openContextMenu(true);
                        return Clutter.EVENT_STOP;
                    }
                    return Clutter.EVENT_PROPAGATE;
                }),
                this.menu.actor.connect('destroy', () => {
                    this._menuActorDisposed = true;
                    this._menuActorSigIds = [];
                })
            );
        }

        if (this._contextMenu) {
            this._contextMenuSigIds.push(
                this._contextMenu.connect('open-state-changed', (_menu, isOpen) => {
                    if (this._destroyed)
                        return;
                    if (isOpen) {
                        // Temporarily hide `this.menu` as a sourceActor match while the
                        // secondary context menu is open so PopupMenuManager hover-sliding
                        // resolves `this._contextMenu` rather than flipping to `this.menu`.
                        if (this.menu)
                            this.menu.sourceActor = null;
                        this.add_style_pseudo_class('active');
                    } else {
                        if (this.menu)
                            this.menu.sourceActor = this;
                        if (!this.menu?.isOpen)
                            this.remove_style_pseudo_class('active');
                        this._updateAutoHideVisibility();
                    }
                })
            );
        }

        if (this._contextMenu?.actor) {
            this._contextMenuActorSigIds.push(
                this._contextMenu.actor.connect('captured-event', (actor, event) => {
                    if (this._destroyed || !this._contextMenu?.isOpen)
                        return Clutter.EVENT_PROPAGATE;

                    if (
                        event.type() === Clutter.EventType.BUTTON_PRESS &&
                        this._isEventOnIndicator(event, actor)
                    ) {
                        const btn = event.get_button();
                        if (btn === Clutter.BUTTON_PRIMARY) {
                            this._contextMenu.close(false);
                            if (this.menu) {
                                this.menu.sourceActor = this;
                                this.menu.open(true);
                            }
                            return Clutter.EVENT_STOP;
                        }
                        if (btn === Clutter.BUTTON_SECONDARY) {
                            this._contextMenu.close(true);
                            return Clutter.EVENT_STOP;
                        }
                    }

                    return Clutter.EVENT_PROPAGATE;
                }),
                this._contextMenu.actor.connect('destroy', () => {
                    this._contextMenuActorDisposed = true;
                    this._contextMenuActorSigIds = [];
                })
            );
        }

        this._selfSigIds.push(
            this.connect('notify::mapped', () => {
                if (!this._destroyed)
                    this._syncPanelLuminanceClass();
            }),
            this.connect('destroy', () => {
                this._cleanupResources(true);
            })
        );

        try {
            this._themeContext = St.ThemeContext.get_for_stage(global.stage);
            if (this._themeContext) {
                this._themeContextSigId = this._themeContext.connect('changed', () => {
                    if (!this._destroyed)
                        this._syncPanelLuminanceClass();
                });
            }
        } catch (_e) {
            this._themeContext = null;
            this._themeContextSigId = 0;
        }
    }

    _syncPanelLuminanceClass() {
        try {
            if (!Main.panel?.get_stage?.() || !Main.panel.mapped || !this._rootBox?.get_stage?.())
                return;
            const bg = Main.panel.get_theme_node().get_background_color();
            if ((bg.alpha ?? 0) < 40) {
                this._rootBox.remove_style_class_name('on-light-panel');
                return;
            }
            const toLinear = c => {
                const v = c / 255.0;
                return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
            };
            const lum = 0.2126 * toLinear(bg.red) + 0.7152 * toLinear(bg.green) + 0.0722 * toLinear(bg.blue);
            if (lum > 0.45)
                this._rootBox.add_style_class_name('on-light-panel');
            else
                this._rootBox.remove_style_class_name('on-light-panel');
        } catch (_e) {
            // Ignore before stage realization
        }
    }

    handleReport(report) {
        if (this._destroyed)
            return;
        this._hasHydratedOnce = true;
        this._lastReport = report;
        this._lastError = null;
        this._syncPanelPresentation();
        if (this._popoverController)
            this._popoverController.setReport(report);
        else if (this._fallbackItems?.statusItem)
            this._fallbackItems.statusItem.label.set_text(report.label || 'Ready');
    }

    handleError(message) {
        if (this._destroyed)
            return;
        this._hasHydratedOnce = true;
        this._lastError = message;
        this._syncPanelPresentation();
        if (this._popoverController)
            this._popoverController.setError(message);
        else if (this._fallbackItems?.statusItem)
            this._fallbackItems.statusItem.label.set_text(message || 'Collection error');
    }

    handleLoadingChanged(isLoading) {
        if (this._destroyed)
            return;
        this._isLoading = Boolean(isLoading);
        if (this._popoverController)
            this._popoverController.setLoading(this._isLoading);
    }

    async openConfigFile() {
        try {
            const cfgPath = await this._backend.ensureConfigInitialized();
            if (this._destroyed)
                return;
            const uri = GLib.filename_to_uri(cfgPath, null);
            const ctx = global.create_app_launch_context?.(0, -1) ?? null;
            Gio.AppInfo.launch_default_for_uri(uri, ctx);
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to open config: ${e.message}`);
        }
    }

    openExtensionPreferences() {
        try {
            this._extension.openPreferences();
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to open preferences: ${e.message}`);
        }
    }

    _getVisibleProviders() {
        const providers = this._lastReport?.providers ?? [];
        const hidden = this._settings.get_strv('hidden-providers') ?? [];
        return filterVisibleProviders(providers, hidden);
    }

    _updateAutoHideVisibility() {
        const autoHide = this._settings.get_boolean('auto-hide-when-empty');
        if (
            !autoHide ||
            !this._hasHydratedOnce ||
            this.menu?.isOpen ||
            this._contextMenu?.isOpen
        ) {
            this.visible = true;
            return;
        }
        const visibleProviders = this._getVisibleProviders();
        this.visible = visibleProviders.length > 0;
    }

    _syncPanelPresentation() {
        const panelMode = this._settings.get_string('panel-mode') || 'icon';
        const metricMode = this._settings.get_string('metric-mode') === 'remaining'
            ? 'remaining'
            : 'used';
        const isMonospace = this._settings.get_string('font-style') === 'monospace';

        const visibleProviders = this._getVisibleProviders();
        const summary = computePanelSummary(visibleProviders, metricMode);

        let effectiveSeverity = summary.severity;
        if (this._lastError && !this._lastReport)
            effectiveSeverity = 'critical';

        if (isMonospace)
            this._rootBox.add_style_class_name('font-monospace');
        else
            this._rootBox.remove_style_class_name('font-monospace');

        this._syncPanelLuminanceClass();
        this._updateAutoHideVisibility();

        if (panelMode === 'compact-pills') {
            this._iconModeBox.hide();
            this._fullTextLabel.hide();
            this._pillsModeBox.show();
            this._renderCompactPills(summary.pills);
        } else if (panelMode === 'full-text') {
            this._iconModeBox.hide();
            this._pillsModeBox.hide();
            this._fullTextLabel.show();
            this._renderFullText(summary, effectiveSeverity, metricMode, visibleProviders);
        } else {
            this._pillsModeBox.hide();
            this._fullTextLabel.hide();
            this._iconModeBox.show();
            this._renderMinimalIcon(effectiveSeverity);
        }
    }

    _renderMinimalIcon(severity) {
        this._symbolicIcon.remove_style_class_name('is-warning');
        this._symbolicIcon.remove_style_class_name('is-critical');
        this._statusDot.remove_style_class_name('is-warning');
        this._statusDot.remove_style_class_name('is-critical');

        if (severity === 'critical') {
            this._symbolicIcon.add_style_class_name('is-critical');
            this._statusDot.add_style_class_name('is-critical');
            this._statusDot.show();
        } else if (severity === 'warning') {
            this._symbolicIcon.add_style_class_name('is-warning');
            this._statusDot.add_style_class_name('is-warning');
            this._statusDot.show();
        } else {
            this._statusDot.hide();
        }
    }

    _ensurePillSlot(index) {
        while (this._pillPool.length <= index) {
            const wrap = createHBox({
                style_class: 'gas-panel-pill',
                y_align: Clutter.ActorAlign.CENTER,
            });
            const icon = new St.Icon({
                icon_size: 14,
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-panel-pill-icon',
            });
            const label = new St.Label({
                text: '',
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-panel-pill-label',
            });
            wrap.add_child(icon);
            wrap.add_child(label);
            this._pillsModeBox.add_child(wrap);
            this._pillPool.push({wrap, icon, label});
        }
        return this._pillPool[index];
    }

    _renderCompactPills(pills) {
        if (!pills || pills.length === 0) {
            this._pillsEmptyWrap.show();
            this._pillsEmptyLabel.set_text(
                this._lastError && !this._lastReport ? 'ERR' : (this._hasHydratedOnce ? '—' : '…')
            );
            for (const slot of this._pillPool)
                slot.wrap.hide();
            return;
        }

        this._pillsEmptyWrap.hide();
        for (let i = 0; i < pills.length; i++) {
            const item = pills[i];
            const slot = this._ensurePillSlot(i);
            slot.icon.set_gicon(this.getIcon(item.iconName));
            slot.label.set_text(item.percentText);
            slot.label.remove_style_class_name('is-warning');
            slot.label.remove_style_class_name('is-critical');
            if (item.severity === 'critical')
                slot.label.add_style_class_name('is-critical');
            else if (item.severity === 'warning')
                slot.label.add_style_class_name('is-warning');
            slot.wrap.show();
        }

        for (let i = pills.length; i < this._pillPool.length; i++)
            this._pillPool[i].wrap.hide();
    }

    _renderFullText(summary, severity, metricMode, visibleProviders) {
        let text;
        if (!this._hasHydratedOnce) {
            text = 'Agents: …';
        } else if (this._lastError && !this._lastReport) {
            text = 'Agents: ERR';
        } else if (
            metricMode === 'used' &&
            this._lastReport?.label &&
            visibleProviders.length === (this._lastReport.providers?.length ?? 0)
        ) {
            text = this._lastReport.label;
        } else {
            text = summary.fullText;
        }

        this._fullTextLabel.set_text(text || 'Agents: —');
        this._fullTextLabel.remove_style_class_name('is-warning');
        this._fullTextLabel.remove_style_class_name('is-critical');
        if (severity === 'critical')
            this._fullTextLabel.add_style_class_name('is-critical');
        else if (severity === 'warning')
            this._fullTextLabel.add_style_class_name('is-warning');
    }

    _cleanupResources(fromDestroySignal = false) {
        if (this._destroyed)
            return;
        this._destroyed = true;

        if (this.menu)
            this.menu.sourceActor = this;

        if (this._quitIdleId !== 0) {
            try {
                GLib.Source.remove(this._quitIdleId);
            } catch (_e) {}
            this._quitIdleId = 0;
        }

        for (const sigId of this._settingsSigIds)
            this._settings.disconnect(sigId);
        this._settingsSigIds = [];

        if (this.menu) {
            for (const sigId of this._menuSigIds) {
                try {
                    this.menu.disconnect(sigId);
                } catch (_e) {}
            }
            if (this.menu.actor && !this._menuActorDisposed) {
                for (const sigId of this._menuActorSigIds) {
                    try {
                        this.menu.actor.disconnect(sigId);
                    } catch (_e) {}
                }
            }
        }
        this._menuSigIds = [];
        this._menuActorSigIds = [];

        if (this._contextMenu) {
            for (const sigId of this._contextMenuSigIds) {
                try {
                    this._contextMenu.disconnect(sigId);
                } catch (_e) {}
            }
            if (this._contextMenu.actor && !this._contextMenuActorDisposed) {
                for (const sigId of this._contextMenuActorSigIds) {
                    try {
                        this._contextMenu.actor.disconnect(sigId);
                    } catch (_e) {}
                }
            }
        }
        this._contextMenuSigIds = [];
        this._contextMenuActorSigIds = [];

        if (
            !fromDestroySignal &&
            this._secondaryClickGesture &&
            this._secondaryClickGesture.get_actor?.() === this
        ) {
            try {
                this.remove_action(this._secondaryClickGesture);
            } catch (_e) {}
        }
        this._secondaryClickGesture = null;

        if (!fromDestroySignal) {
            for (const sigId of this._selfSigIds) {
                try {
                    this.disconnect(sigId);
                } catch (_e) {}
            }
        }
        this._selfSigIds = [];

        if (this._themeContext && this._themeContextSigId !== 0) {
            try {
                this._themeContext.disconnect(this._themeContextSigId);
            } catch (_e) {}
            this._themeContextSigId = 0;
        }
        this._themeContext = null;

        if (this._popoverController) {
            this._popoverController.destroy();
            this._popoverController = null;
        }

        if (this._contextMenu) {
            if (!this._contextMenuActorDisposed) {
                try {
                    this._contextMenu.close(false);
                } catch (_e) {}
                if (this._contextMenuManager) {
                    try {
                        this._contextMenuManager.removeMenu(this._contextMenu);
                    } catch (_e) {}
                }
                try {
                    this._contextMenu.destroy();
                } catch (_e) {}
            }
            this._contextMenuManager = null;
            this._contextMenu = null;
        }
        this._refreshMenuItem = null;
        this._settingsMenuItem = null;
        this._quitMenuItem = null;

        this._iconCache.clear();
        this._pillPool = [];
    }

    destroy() {
        if (this._contextMenu?.isOpen) {
            try {
                this._contextMenu.close(false);
            } catch (_e) {}
        }
        if (this.menu?.isOpen) {
            try {
                this.menu.close(false);
            } catch (_e) {}
        }
        this._cleanupResources(false);
        super.destroy();
    }
});
