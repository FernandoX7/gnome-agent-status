import Clutter from 'gi://Clutter';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {
    _,
    connectInteractiveButton,
    createHBox,
    createVBox,
    setScrollChild,
} from '../compat.js';
import {
    THEME_BY_ID,
    THEME_CATEGORIES,
    THEME_PRESETS,
} from '../format.js';
import {SegmentedToggle} from './widgets.js';

const CATEGORY_BADGE_LABELS = Object.freeze({
    glass: 'Glass',
    light: 'Light',
    dark: 'Dark',
});

/**
 * Page 4 (`All 24 Themes` dedicated live theme picker):
 * - `‹ Quick Options` back navigation (`Esc` / `BackSpace` / `Alt+Left` returns to `options`)
 * - Category filter tabs (`All` | `Dark` | `Light` | `Glass`)
 * - Scrollable list (or 2-column grid in wide mode) of all 24 curated themes with
 *   dual-swatch (`bgHex` + `accentHex` dot), palette name, category badge, and `✓` mark
 * - Clicking any theme persists `settings.set_string('theme', id)` immediately and keeps
 *   `ThemesPage` open for effortless live comparison
 */
export class ThemesPage {
    constructor({settings, tooltips = null, onBack}) {
        this._settings = settings;
        this._tooltips = tooltips;
        this._activeCategory = 'all';
        this._isWideGrid = false;
        this._rowEntries = new Map();
        this._gridRows = [];

        this.actor = createVBox({
            style_class: 'gas-page gas-themes-page',
            x_expand: true,
            visible: false,
        });

        // Header (`‹ Quick Options` + filtered count)
        const headerRow = createHBox({
            style_class: 'gas-detail-header',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });

        const backInner = createHBox({
            style_class: 'gas-back-btn-inner',
            y_align: Clutter.ActorAlign.CENTER,
        });
        backInner.add_child(new St.Label({
            text: '‹',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-back-chevron',
        }));
        backInner.add_child(new St.Label({
            text: _('Quick Options'),
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-detail-title',
        }));

        this._backBtn = new St.Button({
            style_class: 'gas-back-btn',
            can_focus: true,
            track_hover: true,
            accessible_name: _('Back to Quick Options (Esc)'),
            child: backInner,
        });
        connectInteractiveButton(this._backBtn, () => {
            onBack();
        });
        this._tooltips?.attach(this._backBtn, _('Back to Quick Options (Esc)'));

        const headerSpacer = new St.Widget({x_expand: true});
        this._countLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-updated-label',
        });

        headerRow.add_child(this._backBtn);
        headerRow.add_child(headerSpacer);
        headerRow.add_child(this._countLabel);

        // Category segmented filter bar (`All` | `Dark` | `Light` | `Glass`)
        const filterWrap = createVBox({
            style_class: 'gas-themes-filter-bar',
            x_expand: true,
        });
        this._categoryToggle = new SegmentedToggle({
            options: THEME_CATEGORIES,
            activeId: this._activeCategory,
            onChange: catId => {
                this._activeCategory = catId;
                this._rebuildFilteredLayout();
            },
        });
        filterWrap.add_child(this._categoryToggle.actor);

        // Scrollable themes list
        this._scrollView = new St.ScrollView({
            style_class: 'gas-scroll-view gas-themes-scroll',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: true,
            x_expand: true,
        });

        this._listBox = createVBox({
            style_class: 'gas-themes-list',
            x_expand: true,
        });
        setScrollChild(this._scrollView, this._listBox);

        for (const preset of THEME_PRESETS) {
            const entry = this._createThemeRow(preset);
            this._rowEntries.set(preset.id, entry);
        }

        this.actor.add_child(headerRow);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(filterWrap);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(this._scrollView);

        this._rebuildFilteredLayout();
        this.syncFromSettings();
    }

    _createThemeRow(preset) {
        const inner = createHBox({
            style_class: 'gas-theme-row-inner',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });

        const dualSwatch = createHBox({
            style_class: 'gas-theme-dual-swatch',
            style: `background-color: ${preset.bgHex};`,
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });
        const swatchSpacer = new St.Widget({x_expand: true});
        const accentDot = new St.Widget({
            style_class: 'gas-theme-accent-dot',
            style: `background-color: ${preset.accentHex};`,
            y_align: Clutter.ActorAlign.CENTER,
        });
        dualSwatch.add_child(swatchSpacer);
        dualSwatch.add_child(accentDot);

        const nameLabel = new St.Label({
            text: _(preset.label),
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-theme-row-label',
        });
        nameLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        const catText = _(CATEGORY_BADGE_LABELS[preset.category] || 'Dark');
        const catBadge = new St.Label({
            text: catText,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-theme-cat-badge',
        });

        const checkLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-theme-check',
        });

        inner.add_child(dualSwatch);
        inner.add_child(nameLabel);
        inner.add_child(catBadge);
        inner.add_child(checkLabel);

        const button = new St.Button({
            style_class: 'gas-theme-row',
            can_focus: true,
            track_hover: true,
            x_expand: true,
            accessible_name: `${_(preset.label)} (${catText})`,
            child: inner,
        });

        connectInteractiveButton(button, () => {
            this._settings.set_string('theme', preset.id);
            this.syncFromSettings();
        });

        return {
            preset,
            button,
            checkLabel,
        };
    }

    _detachAllButtons() {
        for (const {button} of this._rowEntries.values()) {
            const parent = button.get_parent();
            if (parent)
                parent.remove_child(button);
        }
        for (const row of this._gridRows)
            row.destroy();
        this._gridRows = [];
    }

    _getFilteredPresets() {
        if (this._activeCategory === 'all')
            return THEME_PRESETS;
        return THEME_PRESETS.filter(p => p.category === this._activeCategory);
    }

    _rebuildFilteredLayout() {
        this._detachAllButtons();

        const filtered = this._getFilteredPresets();
        const total = filtered.length;
        this._countLabel.set_text(`${total} ${total === 1 ? _('theme') : _('themes')}`);

        if (this._isWideGrid && total >= 2) {
            for (let i = 0; i < total; i += 2) {
                const gridRow = createHBox({
                    style_class: 'gas-themes-grid-row',
                    x_expand: true,
                });
                if (gridRow.layout_manager)
                    gridRow.layout_manager.homogeneous = true;

                const firstEntry = this._rowEntries.get(filtered[i].id);
                if (firstEntry) {
                    firstEntry.button.show();
                    gridRow.add_child(firstEntry.button);
                }

                if (i + 1 < total) {
                    const secondEntry = this._rowEntries.get(filtered[i + 1].id);
                    if (secondEntry) {
                        secondEntry.button.show();
                        gridRow.add_child(secondEntry.button);
                    }
                } else {
                    gridRow.add_child(new St.Widget({x_expand: true}));
                }

                this._listBox.add_child(gridRow);
                this._gridRows.push(gridRow);
            }
            return;
        }

        for (const preset of filtered) {
            const entry = this._rowEntries.get(preset.id);
            if (!entry)
                continue;
            entry.button.show();
            this._listBox.add_child(entry.button);
        }
    }

    setWideGrid(isWideGrid) {
        const next = Boolean(isWideGrid);
        if (this._isWideGrid === next)
            return;
        this._isWideGrid = next;
        this._rebuildFilteredLayout();
    }

    syncFromSettings() {
        const raw = this._settings.get_string('theme') || 'midnight-slate';
        const activeThemeId = THEME_BY_ID[raw] ? raw : 'midnight-slate';

        for (const [id, entry] of this._rowEntries.entries()) {
            const isActive = id === activeThemeId;
            entry.checkLabel.set_text(isActive ? '✓' : '');
            if (isActive)
                entry.button.add_style_class_name('is-active');
            else
                entry.button.remove_style_class_name('is-active');
        }
    }

    focusBackButton() {
        this._backBtn?.grab_key_focus?.();
    }

    handleKeyPress(event) {
        const symbol = event.get_key_symbol();
        if (symbol !== Clutter.KEY_Down && symbol !== Clutter.KEY_Up)
            return Clutter.EVENT_PROPAGATE;

        const filtered = this._getFilteredPresets();
        const focusables = filtered
            .map(p => this._rowEntries.get(p.id)?.button)
            .filter(btn => btn && btn.visible);
        if (focusables.length === 0)
            return Clutter.EVENT_PROPAGATE;

        const currentFocus = global.stage?.get_key_focus?.() ?? null;
        const curIdx = focusables.indexOf(currentFocus);
        let nextIdx;
        if (curIdx === -1) {
            nextIdx = symbol === Clutter.KEY_Down ? 0 : focusables.length - 1;
        } else if (symbol === Clutter.KEY_Down) {
            nextIdx = (curIdx + 1) % focusables.length;
        } else {
            nextIdx = (curIdx - 1 + focusables.length) % focusables.length;
        }

        focusables[nextIdx]?.grab_key_focus?.();
        return Clutter.EVENT_STOP;
    }

    destroy() {
        this._detachAllButtons();
        for (const {button} of this._rowEntries.values())
            button.destroy();
        this._rowEntries.clear();
    }
}
