import Clutter from 'gi://Clutter';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {
    connectInteractiveButton,
    createHBox,
    createVBox,
    setScrollChild,
} from '../compat.js';
import {
    FONT_STYLES,
    METRIC_MODES,
    PANEL_MODES,
    THEME_PRESETS,
} from '../format.js';
import {
    SegmentedToggle,
    createFooterRowButton,
} from './widgets.js';

/**
 * Page 3 (`Quick Options` via `⚙`):
 * - Panel Mode (`Minimal Icon`, `Compact Pills`, `Full Text`)
 * - Theme Preset (all 8 built-in themes with live preview)
 * - Font Style (`System Tabular`, `Monospace`)
 * - Metric (`% Used`, `% Remaining`)
 * - Auto-Hide When Empty toggle
 * - `Manage Custom Account Locations…` button calling `openPreferences()`
 */
export class QuickOptionsPage {
    constructor({settings, tooltips = null, onBack, onOpenPreferences}) {
        this._settings = settings;
        this._tooltips = tooltips;
        this._themeButtons = new Map();

        this.actor = createVBox({
            style_class: 'gas-page gas-options-page',
            x_expand: true,
            visible: false,
        });

        // Header
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
            text: 'Quick Options',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-detail-title',
        }));

        const backBtn = new St.Button({
            style_class: 'gas-back-btn',
            can_focus: true,
            track_hover: true,
            accessible_name: 'Back to All Agents (Esc)',
            child: backInner,
        });
        this._backBtn = backBtn;
        connectInteractiveButton(backBtn, () => {
            onBack();
        });
        this._tooltips?.attach(backBtn, 'Back to All Agents (Esc)');
        headerRow.add_child(backBtn);

        // Scrollable options body
        this._scrollView = new St.ScrollView({
            style_class: 'gas-scroll-view gas-options-scroll',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: true,
            x_expand: true,
        });

        const bodyBox = createVBox({
            style_class: 'gas-options-body',
            x_expand: true,
        });

        // 1. Panel Mode
        bodyBox.add_child(this._createSectionLabel('Top Bar Mode'));
        this._panelModeToggle = new SegmentedToggle({
            options: PANEL_MODES,
            activeId: this._settings.get_string('panel-mode') || 'icon',
            onChange: id => this._settings.set_string('panel-mode', id),
        });
        bodyBox.add_child(this._panelModeToggle.actor);

        // 2. Theme Preset (8 themes arranged in 2-column rows)
        bodyBox.add_child(this._createSectionLabel('Theme Preset'));
        const themeGrid = createVBox({
            style_class: 'gas-theme-grid',
            x_expand: true,
        });

        for (let i = 0; i < THEME_PRESETS.length; i += 2) {
            const row = createHBox({
                style_class: 'gas-theme-grid-row',
                x_expand: true,
            });
            if (row.layout_manager)
                row.layout_manager.homogeneous = true;
            for (const preset of THEME_PRESETS.slice(i, i + 2)) {
                const chip = this._createThemeChip(preset);
                row.add_child(chip);
            }
            themeGrid.add_child(row);
        }
        bodyBox.add_child(themeGrid);

        // 3. Font Style
        bodyBox.add_child(this._createSectionLabel('Font Style'));
        this._fontToggle = new SegmentedToggle({
            options: FONT_STYLES,
            activeId: this._settings.get_string('font-style') || 'system',
            onChange: id => this._settings.set_string('font-style', id),
        });
        bodyBox.add_child(this._fontToggle.actor);

        // 4. Quota Metric
        bodyBox.add_child(this._createSectionLabel('Quota Metric'));
        this._metricToggle = new SegmentedToggle({
            options: METRIC_MODES,
            activeId: this._settings.get_string('metric-mode') || 'used',
            onChange: id => this._settings.set_string('metric-mode', id),
        });
        bodyBox.add_child(this._metricToggle.actor);

        // 5. Auto-Hide When Empty Toggle
        const autoHideRow = createHBox({
            style_class: 'gas-option-switch-row',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        const autoHideTitle = new St.Label({
            text: 'Auto-Hide When Empty',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-option-switch-label',
        });
        this._autoHidePillLabel = new St.Label({
            text: 'Off',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-toggle-pill-text',
        });
        this._autoHideBtn = new St.Button({
            style_class: 'gas-toggle-pill',
            can_focus: true,
            track_hover: true,
            y_align: Clutter.ActorAlign.CENTER,
            child: this._autoHidePillLabel,
        });
        connectInteractiveButton(this._autoHideBtn, () => {
            const current = this._settings.get_boolean('auto-hide-when-empty');
            this._settings.set_boolean('auto-hide-when-empty', !current);
        });
        this._tooltips?.attach(
            this._autoHideBtn,
            'Hide top-bar indicator when no agents are active'
        );
        autoHideRow.add_child(autoHideTitle);
        autoHideRow.add_child(this._autoHideBtn);
        bodyBox.add_child(autoHideRow);

        setScrollChild(this._scrollView, bodyBox);

        // Footer
        const footerBox = createVBox({
            style_class: 'gas-footer-section',
            x_expand: true,
        });
        const customLocationsBtn = createFooterRowButton({
            title: 'Manage Custom Account Locations…',
            onActivate: () => onOpenPreferences(),
        });
        footerBox.add_child(customLocationsBtn.actor);

        this.actor.add_child(headerRow);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(this._scrollView);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(footerBox);

        this.syncFromSettings();
    }

    _createSectionLabel(text) {
        return new St.Label({
            text,
            x_align: Clutter.ActorAlign.START,
            style_class: 'gas-option-section-title',
        });
    }

    _createThemeChip(preset) {
        const inner = createHBox({
            style_class: 'gas-theme-chip-inner',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        const swatch = new St.Widget({
            style_class: 'gas-theme-swatch',
            style: `background-color: ${preset.swatchHex};`,
            y_align: Clutter.ActorAlign.CENTER,
        });
        const label = new St.Label({
            text: preset.label,
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-theme-chip-label',
        });
        label.clutter_text.ellipsize = Pango.EllipsizeMode.END;
        inner.add_child(swatch);
        inner.add_child(label);

        const btn = new St.Button({
            style_class: 'gas-theme-chip',
            can_focus: true,
            track_hover: true,
            x_expand: true,
            child: inner,
        });
        connectInteractiveButton(btn, () => {
            this._settings.set_string('theme', preset.id);
        });

        this._themeButtons.set(preset.id, btn);
        return btn;
    }

    focusBackButton() {
        this._backBtn?.grab_key_focus?.();
    }

    syncFromSettings() {
        const panelMode = this._settings.get_string('panel-mode') || 'icon';
        const themeId = this._settings.get_string('theme') || 'midnight-slate';
        const fontStyle = this._settings.get_string('font-style') || 'system';
        const metricMode = this._settings.get_string('metric-mode') || 'used';
        const autoHide = this._settings.get_boolean('auto-hide-when-empty');

        this._panelModeToggle.setActive(panelMode);
        this._fontToggle.setActive(fontStyle);
        this._metricToggle.setActive(metricMode);

        for (const [id, btn] of this._themeButtons.entries()) {
            if (id === themeId)
                btn.add_style_class_name('is-active');
            else
                btn.remove_style_class_name('is-active');
        }

        this._autoHidePillLabel.set_text(autoHide ? 'On' : 'Off');
        if (autoHide)
            this._autoHideBtn.add_style_class_name('is-active');
        else
            this._autoHideBtn.remove_style_class_name('is-active');
    }
}
