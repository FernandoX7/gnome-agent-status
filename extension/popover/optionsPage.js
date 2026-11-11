import Clutter from 'gi://Clutter';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {
    _,
    connectInteractiveButton,
    createHBox,
    createVBox,
    getSettingStringSafe,
    setScrollChild,
    setSettingStringSafe,
} from '../compat.js';
import {
    FONT_STYLES,
    METRIC_MODES,
    PANEL_MODES,
    POPOVER_LAYOUTS,
    QUICK_THEME_IDS,
    THEME_BY_ID,
    getThemePalette,
} from '../format.js';
import {
    SegmentedToggle,
    createFooterRowButton,
} from './widgets.js';

/**
 * Page 3 (`Quick Options` via `⚙`):
 * - Panel Mode (`Minimal Icon`, `Compact Pills`, `Full Text`)
 * - Popover Layout (`Compact List`, `Auto 2-Column`, `2-Column Wide`)
 * - Theme Preset (4-preset quick strip + `All 24 Themes · <Current Theme Label> ›` nav card)
 * - Font Style (`System Tabular`, `Monospace`)
 * - Quota Metric (`% Used`, `% Remaining`)
 * - Auto-Hide When Empty toggle
 * - `Manage Custom Account Locations…` button calling `openPreferences()`
 */
export class QuickOptionsPage {
    constructor({
        settings,
        tooltips = null,
        onBack,
        onOpenThemes,
        onLayoutChanged,
        onOpenPreferences,
    }) {
        this._settings = settings;
        this._tooltips = tooltips;
        this._onOpenThemes = onOpenThemes;
        this._onLayoutChanged = onLayoutChanged;
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
            text: _('Quick Options'),
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-detail-title',
        }));

        const backBtn = new St.Button({
            style_class: 'gas-back-btn',
            can_focus: true,
            track_hover: true,
            accessible_name: _('Back to All Agents (Esc)'),
            child: backInner,
        });
        this._backBtn = backBtn;
        connectInteractiveButton(backBtn, () => {
            onBack();
        });
        this._tooltips?.attach(backBtn, _('Back to All Agents (Esc)'));
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

        // 1. Top Bar Mode
        bodyBox.add_child(this._createSectionLabel(_('Top Bar Mode')));
        this._panelModeToggle = new SegmentedToggle({
            options: PANEL_MODES,
            activeId: this._settings.get_string('panel-mode') || 'icon',
            onChange: id => this._settings.set_string('panel-mode', id),
        });
        bodyBox.add_child(this._panelModeToggle.actor);

        // 2. Popover Layout (`list` | `auto-grid` | `grid`)
        bodyBox.add_child(this._createSectionLabel(_('Layout')));
        this._layoutToggle = new SegmentedToggle({
            options: POPOVER_LAYOUTS,
            activeId: getSettingStringSafe(this._settings, 'popover-layout', 'list'),
            onChange: id => {
                setSettingStringSafe(this._settings, 'popover-layout', id);
                this._onLayoutChanged?.(id);
            },
        });
        bodyBox.add_child(this._layoutToggle.actor);

        // 3. Theme Preset (4-preset quick strip + `All 24 Themes · <Current Theme> ›`)
        bodyBox.add_child(this._createSectionLabel(_('Theme Preset')));
        const themeGrid = createVBox({
            style_class: 'gas-theme-grid',
            x_expand: true,
        });

        const quickPresets = QUICK_THEME_IDS
            .map(id => THEME_BY_ID[id])
            .filter(Boolean);

        for (let i = 0; i < quickPresets.length; i += 2) {
            const row = createHBox({
                style_class: 'gas-theme-grid-row',
                x_expand: true,
            });
            if (row.layout_manager)
                row.layout_manager.homogeneous = true;
            for (const preset of quickPresets.slice(i, i + 2)) {
                const chip = this._createThemeChip(preset);
                row.add_child(chip);
            }
            themeGrid.add_child(row);
        }

        const allThemesInner = createHBox({
            style_class: 'gas-all-themes-inner',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._activeThemeDualSwatch = createHBox({
            style_class: 'gas-theme-dual-swatch',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._activeThemeAccentDot = new St.Widget({
            style_class: 'gas-theme-accent-dot',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._activeThemeDualSwatch.add_child(new St.Widget({x_expand: true}));
        this._activeThemeDualSwatch.add_child(this._activeThemeAccentDot);

        this._allThemesTitleLabel = new St.Label({
            text: _('All 24 Themes'),
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-all-themes-title',
        });
        this._allThemesCurrentLabel = new St.Label({
            text: '',
            x_expand: true,
            x_align: Clutter.ActorAlign.END,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-all-themes-current',
        });
        this._allThemesCurrentLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;
        const allThemesChevron = new St.Label({
            text: '›',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-chevron',
        });

        allThemesInner.add_child(this._activeThemeDualSwatch);
        allThemesInner.add_child(this._allThemesTitleLabel);
        allThemesInner.add_child(this._allThemesCurrentLabel);
        allThemesInner.add_child(allThemesChevron);

        this._allThemesBtn = new St.Button({
            style_class: 'gas-all-themes-btn',
            can_focus: true,
            track_hover: true,
            x_expand: true,
            child: allThemesInner,
        });
        connectInteractiveButton(this._allThemesBtn, () => {
            this._onOpenThemes?.();
        });
        this._tooltips?.attach(
            this._allThemesBtn,
            _('Browse all 24 curated Dark, Light & Liquid Glass themes')
        );
        themeGrid.add_child(this._allThemesBtn);
        bodyBox.add_child(themeGrid);

        // 4. Font Style
        bodyBox.add_child(this._createSectionLabel(_('Font Style')));
        this._fontToggle = new SegmentedToggle({
            options: FONT_STYLES,
            activeId: this._settings.get_string('font-style') || 'system',
            onChange: id => this._settings.set_string('font-style', id),
        });
        bodyBox.add_child(this._fontToggle.actor);

        // 5. Quota Metric
        bodyBox.add_child(this._createSectionLabel(_('Quota Metric')));
        this._metricToggle = new SegmentedToggle({
            options: METRIC_MODES,
            activeId: this._settings.get_string('metric-mode') || 'used',
            onChange: id => this._settings.set_string('metric-mode', id),
        });
        bodyBox.add_child(this._metricToggle.actor);

        // 6. Auto-Hide When Empty Toggle
        const autoHideRow = createHBox({
            style_class: 'gas-option-switch-row',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        const autoHideTitle = new St.Label({
            text: _('Auto-Hide When Empty'),
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-option-switch-label',
        });
        this._autoHidePillLabel = new St.Label({
            text: _('Off'),
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
            _('Hide top-bar indicator when no agents are active')
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
            title: _('Manage Custom Account Locations…'),
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
            text: _(preset.label),
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
        const layoutMode = getSettingStringSafe(this._settings, 'popover-layout', 'list');
        const rawThemeId = this._settings.get_string('theme') || 'midnight-slate';
        const activeTheme = getThemePalette(rawThemeId);
        const fontStyle = this._settings.get_string('font-style') || 'system';
        const metricMode = this._settings.get_string('metric-mode') || 'used';
        const autoHide = this._settings.get_boolean('auto-hide-when-empty');

        this._panelModeToggle.setActive(panelMode);
        this._layoutToggle.setActive(layoutMode);
        this._fontToggle.setActive(fontStyle);
        this._metricToggle.setActive(metricMode);

        for (const [id, btn] of this._themeButtons.entries()) {
            if (id === activeTheme.id)
                btn.add_style_class_name('is-active');
            else
                btn.remove_style_class_name('is-active');
        }

        this._activeThemeDualSwatch.style = `background-color: ${activeTheme.bgHex};`;
        this._activeThemeAccentDot.style = `background-color: ${activeTheme.accentHex};`;
        this._allThemesCurrentLabel.set_text(`· ${_(activeTheme.label)}`);
        this._allThemesBtn.accessible_name = `${_('All 24 Themes')} · ${_(activeTheme.label)}`;

        this._autoHidePillLabel.set_text(autoHide ? _('On') : _('Off'));
        if (autoHide)
            this._autoHideBtn.add_style_class_name('is-active');
        else
            this._autoHideBtn.remove_style_class_name('is-active');
    }
}
