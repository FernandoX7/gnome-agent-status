import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as MessageTray from 'resource:///org/gnome/shell/ui/messageTray.js';

import {
    getSettingBoolSafe,
    getSettingStrvSafe,
} from './compat.js';
import {
    DEFAULT_HIDDEN_PROVIDERS,
    DEFAULT_PROVIDER_ORDER,
    evaluateQuotaNotificationStep,
} from './format.js';

const MAX_HISTORY_ENTRIES = 25;

/**
 * Tracks quota threshold crossings (`80%` warning, `90%` critical) and fresh quota resets
 * across visible providers, dispatching calm-technology GNOME 45–50 desktop notifications
 * without ever spamming on initial login or screen-unlock hydration.
 */
export class QuotaNotificationTracker {
    /**
     * @param {{
     *   extension?: {path?: string}|null,
     *   settings?: Gio.Settings|null,
     *   dispatchFn?: ((alert: {kind: string, providerId: string, title: string, body: string, urgency: string}) => void)|null
     * }} [params]
     */
    constructor({extension = null, settings = null, dispatchFn = null} = {}) {
        this._extension = extension;
        this._settings = settings;
        this._dispatchFn = dispatchFn;

        this._destroyed = false;
        this._hasSeededBaseline = false;
        this._bucketStates = new Map();
        this._liveNotifications = new Set();
        this._dispatchedLog = [];
        this._robotIcon = null;
    }

    get dispatchedHistory() {
        return [...this._dispatchedLog];
    }

    _getRobotIcon() {
        if (this._robotIcon)
            return this._robotIcon;

        if (this._extension?.path) {
            const svgPath = GLib.build_filenamev([
                this._extension.path,
                'icons',
                'agent-status-symbolic.svg',
            ]);
            this._robotIcon = new Gio.FileIcon({
                file: Gio.File.new_for_path(svgPath),
            });
            return this._robotIcon;
        }

        this._robotIcon = new Gio.ThemedIcon({name: 'dialog-information-symbolic'});
        return this._robotIcon;
    }

    /**
     * Evaluates a `StatusReport` snapshot and dispatches desktop notifications for genuine
     * live threshold crossings or quota window resets.
     *
     * @param {object} report
     * @param {{isInitialHydration?: boolean, nowEpoch?: number}} [options]
     * @returns {Array<{kind: string, providerId: string, title: string, body: string, urgency: string}>}
     */
    handleReport(
        report,
        {
            isInitialHydration = false,
            nowEpoch = Math.floor(Date.now() / 1000),
        } = {}
    ) {
        if (this._destroyed || !report || typeof report !== 'object')
            return [];

        const seedOnly = Boolean(isInitialHydration || !this._hasSeededBaseline);
        this._hasSeededBaseline = true;

        const hiddenProviderIds = this._settings
            ? getSettingStrvSafe(this._settings, 'hidden-providers', DEFAULT_HIDDEN_PROVIDERS)
            : [];
        const providerOrder = getSettingStrvSafe(
            this._settings,
            'provider-order',
            DEFAULT_PROVIDER_ORDER
        );
        const notifyOnWarning = getSettingBoolSafe(this._settings, 'notify-on-warning', false);
        const notifyOnCritical = getSettingBoolSafe(this._settings, 'notify-on-critical', true);
        const notifyOnReset = getSettingBoolSafe(this._settings, 'notify-on-reset', true);

        const alerts = evaluateQuotaNotificationStep(this._bucketStates, report, {
            isInitialHydration: seedOnly,
            nowEpoch,
            hiddenProviderIds,
            providerOrder,
            notifyOnWarning,
            notifyOnCritical,
            notifyOnReset,
        });

        for (const alert of alerts) {
            if (this._destroyed)
                break;

            this._dispatchedLog.push({...alert, atEpoch: nowEpoch});
            if (this._dispatchedLog.length > MAX_HISTORY_ENTRIES)
                this._dispatchedLog.shift();

            this._dispatchDesktopNotification(alert);
        }

        return alerts;
    }

    _dispatchDesktopNotification(alert) {
        if (this._destroyed)
            return;

        if (typeof this._dispatchFn === 'function') {
            this._dispatchFn(alert);
            return;
        }

        const {title, body, urgency} = alert;

        try {
            if (typeof MessageTray.getSystemSource === 'function') {
                const source = MessageTray.getSystemSource();
                const isCritical = urgency === 'critical';
                const trayUrgency = isCritical
                    ? (MessageTray.Urgency?.CRITICAL ?? 3)
                    : (MessageTray.Urgency?.NORMAL ?? 1);
                const notification = new MessageTray.Notification({
                    source,
                    title,
                    body,
                    gicon: this._getRobotIcon(),
                    urgency: trayUrgency,
                    isTransient: !isCritical,
                });
                this._liveNotifications.add(notification);
                notification.connect('destroy', () => {
                    this._liveNotifications.delete(notification);
                });
                source.addNotification(notification);
                return;
            }
        } catch (e) {
            console.error(`[gnome-agent-status] MessageTray dispatch fallback: ${e.message}`);
        }

        try {
            Main.notify(title, body);
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to dispatch notification: ${e.message}`);
        }
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;

        for (const notification of [...this._liveNotifications]) {
            try {
                notification.destroy();
            } catch (_e) {
                // Ignore already-dismissed notifications
            }
        }
        this._liveNotifications.clear();
        this._bucketStates.clear();
        this._dispatchedLog = [];
        this._robotIcon = null;
        this._settings = null;
        this._extension = null;
        this._dispatchFn = null;
    }
}
