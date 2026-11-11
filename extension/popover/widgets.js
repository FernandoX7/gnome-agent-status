import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {
    areAnimationsEnabled,
    connectInteractiveButton,
    createHBox,
} from '../compat.js';
import {
    THEME_PRESETS,
    getBarFillRgba,
    getThemePalette,
} from '../format.js';

/**
 * Draws a horizontal stadium/capsule path of `(x, y, w, h)` with corner radius `r` onto Cairo `cr`.
 *
 * @param {object} cr Cairo context
 * @param {number} x
 * @param {number} y
 * @param {number} w
 * @param {number} h
 */
function traceCapsulePath(cr, x, y, w, h) {
    const r = Math.min(h / 2, w / 2);
    if (r <= 0)
        return;

    cr.newSubPath();
    cr.arc(x + w - r, y + r, r, -Math.PI / 2, Math.PI / 2);
    cr.arc(x + r, y + r, r, Math.PI / 2, (3 * Math.PI) / 2);
    cr.closePath();
}

/**
 * Anti-aliased Cairo capsule progress bar (`St.DrawingArea`).
 *
 * Guardrails enforced:
 * - Every `'repaint'` callback wraps drawing in `try { ... } finally { cr.$dispose(); }`
 * - `queue_repaint()` is strictly gated on `isMenuOpen()` so closed popovers never wake Mutter
 */
export class CapsuleBar {
    /**
     * @param {{
     *   width?: number,
     *   height?: number,
     *   xExpand?: boolean,
     *   isHero?: boolean,
     *   isMenuOpen: () => boolean
     * }} params
     */
    constructor({width = 44, height = 5, xExpand = false, isHero = false, isMenuOpen}) {
        this._isHero = Boolean(isHero);
        this._isMenuOpen = isMenuOpen;

        this._fillFraction = 0;
        this._severity = 'normal';
        this._themeId = 'midnight-slate';

        const areaProps = {
            style_class: isHero ? 'gas-hero-bar-area' : 'gas-mini-bar-area',
            x_expand: Boolean(xExpand),
            y_align: Clutter.ActorAlign.CENTER,
            height,
        };
        if (!xExpand && width > 0)
            areaProps.width = width;

        this.actor = new St.DrawingArea(areaProps);
        this._repaintSigId = this.actor.connect('repaint', area => {
            this._onRepaint(area);
        });
    }

    _onRepaint(area) {
        const cr = area.get_context();
        try {
            const [w, h] = area.get_surface_size();
            if (w <= 0 || h <= 0)
                return;

            const palette = getThemePalette(this._themeId);
            const trackRgba = this._isHero ? palette.heroTrackRgba : palette.trackRgba;

            traceCapsulePath(cr, 0, 0, w, h);
            cr.setSourceRGBA(trackRgba[0], trackRgba[1], trackRgba[2], trackRgba[3]);
            cr.fill();

            if (this._fillFraction > 0.004) {
                const rawFillW = w * Math.min(1, Math.max(0, this._fillFraction));
                const fillW = Math.min(w, Math.max(h, rawFillW));
                const fillRgba = getBarFillRgba(this._severity, this._themeId);

                traceCapsulePath(cr, 0, 0, fillW, h);
                cr.setSourceRGBA(fillRgba[0], fillRgba[1], fillRgba[2], fillRgba[3]);
                cr.fill();
            }
        } finally {
            cr.$dispose();
        }
    }

    /**
     * Updates bar fraction, severity, and theme, queueing a repaint only when the menu is open.
     *
     * @param {{fillFraction: number, severity: 'normal'|'warning'|'critical', themeId: string}} state
     */
    update({fillFraction, severity, themeId}) {
        const clamped = Math.max(0, Math.min(1, Number(fillFraction) || 0));
        const nextSeverity = severity || 'normal';
        const nextTheme = themeId || 'midnight-slate';

        if (
            Math.abs(this._fillFraction - clamped) < 0.001 &&
            this._severity === nextSeverity &&
            this._themeId === nextTheme
        ) {
            return;
        }

        this._fillFraction = clamped;
        this._severity = nextSeverity;
        this._themeId = nextTheme;
        this.repaintIfOpen();
    }

    repaintIfOpen() {
        if (this._isMenuOpen?.() && this.actor.visible)
            this.actor.queue_repaint();
    }

    destroy() {
        if (this._repaintSigId) {
            this.actor.disconnect(this._repaintSigId);
            this._repaintSigId = 0;
        }
    }
}

/**
 * Circular brand icon avatar badge (`28px` regular or `24px` compact) preserving full SVG colors.
 */
export class BrandAvatar {
    /**
     * @param {{
     *   gicon: import('gi://Gio').Icon,
     *   compact?: boolean
     * }} params
     */
    constructor({gicon, compact = false}) {
        this._icon = new St.Icon({
            gicon,
            icon_size: compact ? 14 : 16,
            style_class: 'gas-brand-icon',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });

        this.actor = new St.Bin({
            style_class: compact ? 'gas-avatar-ring gas-avatar-ring-sm' : 'gas-avatar-ring',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.START,
            child: this._icon,
        });
    }

    setCompact(compact) {
        this._icon.icon_size = compact ? 14 : 16;
        if (compact) {
            this.actor.add_style_class_name('gas-avatar-ring-sm');
            this.actor.y_align = Clutter.ActorAlign.CENTER;
        } else {
            this.actor.remove_style_class_name('gas-avatar-ring-sm');
            this.actor.y_align = Clutter.ActorAlign.START;
        }
    }

    setGIcon(gicon) {
        this._icon.set_gicon(gicon);
    }
}

/**
 * Multi-option pill segmented control (`Detailed | Compact`, `System Tabular | Monospace`, etc.)
 * that returns `Clutter.EVENT_STOP` so clicking tabs never dismisses the popover.
 */
export class SegmentedToggle {
    /**
     * @param {{
     *   options: ReadonlyArray<{id: string, label: string}>,
     *   activeId: string,
     *   onChange: (id: string) => void
     * }} params
     */
    constructor({options, activeId, onChange}) {
        this._options = options;
        this._activeId = activeId;
        this._onChange = onChange;
        this._buttons = new Map();

        this.actor = createHBox({
            style_class: 'gas-segmented-track',
            x_expand: true,
        });
        if (this.actor.layout_manager)
            this.actor.layout_manager.homogeneous = true;

        for (const opt of options) {
            const label = new St.Label({
                text: opt.label,
                x_align: Clutter.ActorAlign.CENTER,
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-segmented-label',
            });
            const btn = new St.Button({
                style_class: 'gas-segmented-btn',
                can_focus: true,
                track_hover: true,
                x_expand: true,
                child: label,
            });
            connectInteractiveButton(btn, () => {
                if (this._activeId === opt.id)
                    return;
                this.setActive(opt.id);
                this._onChange?.(opt.id);
            });

            this.actor.add_child(btn);
            this._buttons.set(opt.id, btn);
        }

        this.setActive(activeId);
    }

    setActive(activeId) {
        this._activeId = activeId;
        for (const [id, btn] of this._buttons.entries()) {
            if (id === activeId)
                btn.add_style_class_name('is-active');
            else
                btn.remove_style_class_name('is-active');
        }
    }
}

/**
 * Full-width footer action button with left title and right `›` chevron.
 *
 * @param {{
 *   title: string,
 *   trailingGlyph?: string,
 *   onActivate: () => void
 * }} params
 * @returns {{actor: St.Button, label: St.Label}}
 */
export function createFooterRowButton({title, trailingGlyph = '›', onActivate}) {
    const rowBox = createHBox({
        style_class: 'gas-footer-row-inner',
        x_expand: true,
        y_align: Clutter.ActorAlign.CENTER,
    });

    const label = new St.Label({
        text: title,
        x_expand: true,
        x_align: Clutter.ActorAlign.START,
        y_align: Clutter.ActorAlign.CENTER,
        style_class: 'gas-footer-label',
    });

    const chevron = new St.Label({
        text: trailingGlyph,
        y_align: Clutter.ActorAlign.CENTER,
        style_class: 'gas-chevron',
    });

    rowBox.add_child(label);
    rowBox.add_child(chevron);

    const button = new St.Button({
        style_class: 'gas-footer-btn',
        can_focus: true,
        track_hover: true,
        x_expand: true,
        child: rowBox,
    });

    connectInteractiveButton(button, () => {
        onActivate();
    });

    return {actor: button, label};
}

/**
 * Applies `.is-warning` / `.is-critical` classes cleanly on a percentage or status `St.Label`.
 *
 * @param {St.Label} labelActor
 * @param {'normal'|'warning'|'critical'} severity
 */
export function applySeverityClass(labelActor, severity) {
    labelActor.remove_style_class_name('is-warning');
    labelActor.remove_style_class_name('is-critical');
    if (severity === 'critical')
        labelActor.add_style_class_name('is-critical');
    else if (severity === 'warning')
        labelActor.add_style_class_name('is-warning');
}

const TOOLTIP_HOVER_DELAY_MS = 320;
const TOOLTIP_FADE_DURATION_MS = 110;

/**
 * Reusable accessible tooltip manager for popover controls.
 *
 * Guardrails enforced:
 * - Renders a non-reactive (`reactive: false`, `can_focus: false`) `St.Label` pill on `Main.uiGroup`
 *   so it never intercepts pointer clicks or interferes with `Clutter.ClickGesture`.
 * - Connects only non-event GObject signals (`notify::hover`, `key-focus-in`, `key-focus-out`,
 *   `clicked`, `notify::mapped`, `destroy`) — zero legacy button-press/release handlers.
 * - Opacity-only fade (`110ms` when desktop animations are enabled, `0ms` otherwise) with zero
 *   `scale` transforms anywhere.
 * - Deterministically removes any pending `GLib.timeout_add` source on click, leave, page change,
 *   menu close, or `destroy()`.
 */
export class PopoverTooltipManager {
    /**
     * @param {{
     *   desktopSettings?: import('gi://Gio').Settings|null,
     *   isMenuOpen?: () => boolean
     * }} [params]
     */
    constructor({desktopSettings = null, isMenuOpen = null} = {}) {
        this._desktopSettings = desktopSettings;
        this._isMenuOpen = isMenuOpen;

        this._destroyed = false;
        this._labelDisposed = false;
        this._delayTimerId = 0;
        this._activeTarget = null;
        this._activeTextOrFn = null;
        this._entries = new Map();

        this._label = new St.Label({
            text: '',
            style_class: 'gas-tooltip theme-midnight-slate',
            reactive: false,
            can_focus: false,
            visible: false,
            opacity: 0,
        });
        this._labelDestroySigId = this._label.connect('destroy', () => {
            this._labelDisposed = true;
            this._label = null;
        });

        Main.uiGroup.add_child(this._label);
    }

    get actor() {
        return this._label;
    }

    /**
     * Synchronizes the tooltip's theme and typography classes with the popover root.
     *
     * @param {{themeId: string, isLightSurface?: boolean, isMonospace?: boolean}} state
     */
    setTheme({themeId, isLightSurface = false, isMonospace = false}) {
        if (this._destroyed || !this._label || this._labelDisposed)
            return;

        for (const preset of THEME_PRESETS) {
            const cls = `theme-${preset.id}`;
            if (preset.id === themeId)
                this._label.add_style_class_name(cls);
            else
                this._label.remove_style_class_name(cls);
        }

        if (isLightSurface)
            this._label.add_style_class_name('is-light-surface');
        else
            this._label.remove_style_class_name('is-light-surface');

        if (isMonospace)
            this._label.add_style_class_name('font-monospace');
        else
            this._label.remove_style_class_name('font-monospace');
    }

    /**
     * Attaches a delayed hover & keyboard-focus tooltip to an interactive `St.Button` / actor.
     *
     * @param {Clutter.Actor} actor
     * @param {string | (() => string)} textOrFn
     */
    attach(actor, textOrFn) {
        if (this._destroyed || !actor)
            return;

        this.detach(actor);

        const sigIds = [
            actor.connect('notify::hover', () => {
                if (this._destroyed)
                    return;
                if (actor.hover) {
                    this._scheduleShow(actor, textOrFn);
                } else if (!actor.has_key_focus?.()) {
                    if (this._activeTarget === actor)
                        this.hide();
                    else
                        this._clearTimerFor(actor);
                }
            }),
            actor.connect('key-focus-in', () => {
                if (!this._destroyed)
                    this._scheduleShow(actor, textOrFn);
            }),
            actor.connect('key-focus-out', () => {
                if (!this._destroyed && !actor.hover) {
                    if (this._activeTarget === actor)
                        this.hide();
                    else
                        this._clearTimerFor(actor);
                }
            }),
            actor.connect('clicked', () => {
                this.hide();
            }),
            actor.connect('notify::mapped', () => {
                if (!this._destroyed && !actor.mapped) {
                    if (this._activeTarget === actor)
                        this.hide();
                    else
                        this._clearTimerFor(actor);
                }
            }),
        ];

        const destroySigId = actor.connect('destroy', () => {
            this._entries.delete(actor);
            if (this._activeTarget === actor || this._pendingTarget === actor)
                this.hide();
        });

        this._entries.set(actor, {sigIds, destroySigId, textOrFn});
    }

    detach(actor) {
        const entry = this._entries.get(actor);
        if (!entry)
            return;

        this._entries.delete(actor);
        for (const id of entry.sigIds) {
            try {
                actor.disconnect(id);
            } catch (_e) {}
        }
        try {
            actor.disconnect(entry.destroySigId);
        } catch (_e) {}

        if (this._activeTarget === actor || this._pendingTarget === actor)
            this.hide();
    }

    /**
     * Re-evaluates dynamic tooltip text if a tooltip is currently visible.
     */
    refreshActiveText() {
        if (
            this._destroyed ||
            !this._label ||
            this._labelDisposed ||
            !this._label.visible ||
            !this._activeTarget ||
            !this._activeTextOrFn
        ) {
            return;
        }

        const resolved = this._resolveText(this._activeTextOrFn);
        if (!resolved) {
            this.hide();
            return;
        }
        this._label.set_text(resolved);
        this._positionNearActor(this._activeTarget);
    }

    _resolveText(textOrFn) {
        const raw = typeof textOrFn === 'function' ? textOrFn() : textOrFn;
        return typeof raw === 'string' ? raw.trim() : '';
    }

    _clearTimer() {
        if (this._delayTimerId !== 0) {
            GLib.Source.remove(this._delayTimerId);
            this._delayTimerId = 0;
        }
        this._pendingTarget = null;
    }

    _clearTimerFor(actor) {
        if (this._pendingTarget === actor)
            this._clearTimer();
    }

    _scheduleShow(actor, textOrFn) {
        if (this._destroyed)
            return;
        if (this._isMenuOpen && !this._isMenuOpen())
            return;

        this._clearTimer();
        this._pendingTarget = actor;

        this._delayTimerId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            TOOLTIP_HOVER_DELAY_MS,
            () => {
                this._delayTimerId = 0;
                const target = this._pendingTarget;
                this._pendingTarget = null;

                if (
                    this._destroyed ||
                    !target ||
                    !target.mapped ||
                    (this._isMenuOpen && !this._isMenuOpen()) ||
                    (!target.hover && !target.has_key_focus?.())
                ) {
                    return GLib.SOURCE_REMOVE;
                }

                const text = this._resolveText(textOrFn);
                if (!text) {
                    this.hide();
                    return GLib.SOURCE_REMOVE;
                }

                this._showNow(target, text, textOrFn);
                return GLib.SOURCE_REMOVE;
            }
        );
        GLib.Source.set_name_by_id(this._delayTimerId, '[gnome-agent-status] tooltip-hover-delay');
    }

    _positionNearActor(actor) {
        if (!this._label || this._labelDisposed || !actor?.get_stage?.())
            return;

        const [ax, ay] = actor.get_transformed_position();
        const [aw, ah] = actor.get_transformed_size();
        const [, natW] = this._label.get_preferred_width(-1);
        const [, natH] = this._label.get_preferred_height(natW);
        const tw = Math.ceil(natW);
        const th = Math.ceil(natH);

        let x = Math.round(ax + (aw - tw) / 2);
        let y = Math.round(ay + ah + 6);

        const monitor = Main.layoutManager?.findMonitorForActor?.(actor) ??
            Main.layoutManager?.primaryMonitor ??
            null;
        if (monitor) {
            const minX = monitor.x + 8;
            const maxX = Math.max(minX, monitor.x + monitor.width - tw - 8);
            x = Math.max(minX, Math.min(maxX, x));

            if (y + th > monitor.y + monitor.height - 8)
                y = Math.round(ay - th - 6);
            const minY = monitor.y + 8;
            const maxY = Math.max(minY, monitor.y + monitor.height - th - 8);
            y = Math.max(minY, Math.min(maxY, y));
        }

        this._label.set_position(x, y);
    }

    _showNow(actor, text, textOrFn) {
        if (this._destroyed || !this._label || this._labelDisposed)
            return;

        this._activeTarget = actor;
        this._activeTextOrFn = textOrFn;
        this._label.set_text(text);

        if (this._label.get_parent() === Main.uiGroup)
            Main.uiGroup.set_child_above_sibling(this._label, null);

        this._positionNearActor(actor);

        this._label.remove_all_transitions?.();
        const duration = areAnimationsEnabled(this._desktopSettings) ? TOOLTIP_FADE_DURATION_MS : 0;
        if (duration <= 0 || typeof this._label.ease !== 'function') {
            this._label.opacity = 255;
            this._label.show();
            return;
        }

        this._label.opacity = 0;
        this._label.show();
        this._label.ease({
            opacity: 255,
            duration,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
    }

    hide() {
        this._clearTimer();
        this._activeTarget = null;
        this._activeTextOrFn = null;

        if (!this._label || this._labelDisposed)
            return;

        this._label.remove_all_transitions?.();
        this._label.hide();
        this._label.opacity = 0;
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;

        this.hide();

        for (const [actor, entry] of this._entries.entries()) {
            for (const id of entry.sigIds) {
                try {
                    actor.disconnect(id);
                } catch (_e) {}
            }
            try {
                actor.disconnect(entry.destroySigId);
            } catch (_e) {}
        }
        this._entries.clear();

        if (this._label && !this._labelDisposed) {
            if (this._labelDestroySigId) {
                try {
                    this._label.disconnect(this._labelDestroySigId);
                } catch (_e) {}
                this._labelDestroySigId = 0;
            }
            this._labelDisposed = true;
            this._label.destroy();
        }
        this._label = null;
        this._desktopSettings = null;
    }
}

/**
 * Attaches a standalone `PopoverTooltipManager` instance to a single actor and cleans it up
 * automatically when `actor` is destroyed.
 *
 * @param {Clutter.Actor} actor
 * @param {string | (() => string)} textOrFn
 * @param {import('gi://Gio').Settings|null} [desktopSettings=null]
 * @returns {PopoverTooltipManager}
 */
export function attachTooltip(actor, textOrFn, desktopSettings = null) {
    const manager = new PopoverTooltipManager({desktopSettings});
    manager.attach(actor, textOrFn);
    actor.connect('destroy', () => {
        manager.destroy();
    });
    return manager;
}
