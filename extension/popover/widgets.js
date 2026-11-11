import Clutter from 'gi://Clutter';
import St from 'gi://St';

import {
    connectInteractiveButton,
    createHBox,
} from '../compat.js';
import {
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
