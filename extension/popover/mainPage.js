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
    DENSITY_MODES,
    formatBucketDisplayLabel,
    formatBucketMetric,
    formatCompactAccountExtraNote,
    formatResetCountdown,
    getActiveAccount,
    groupBucketsForDetailedView,
} from '../format.js';
import {
    BrandAvatar,
    CapsuleBar,
    SegmentedToggle,
    applySeverityClass,
    createFooterRowButton,
} from './widgets.js';

/**
 * Reusable provider row card in the Main Usage page.
 * Updates labels, avatars, and Cairo `CapsuleBar`s in-place without destroying actors.
 */
class ProviderCardWidget {
    constructor({providerId, getIcon, isMenuOpen, onSelectProvider}) {
        this.providerId = providerId;
        this._getIcon = getIcon;
        this._isMenuOpen = isMenuOpen;

        this._fullRowPool = [];

        this._avatar = new BrandAvatar({
            gicon: getIcon('agent-status-symbolic'),
            compact: false,
        });

        // Header line (Provider Name · Plan Badge on left, Resets in … pushed flush right)
        this._nameLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-provider-name',
        });
        this._badgeLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-plan-badge',
            visible: false,
        });
        this._badgeLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;
        this._detailedResetLabel = new St.Label({
            text: '',
            x_expand: true,
            x_align: Clutter.ActorAlign.END,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-reset-label',
        });

        this._titleRow = createHBox({
            style_class: 'gas-provider-title-row',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._titleRow.add_child(this._nameLabel);
        this._titleRow.add_child(this._badgeLabel);
        this._titleRow.add_child(this._detailedResetLabel);

        // Detailed buckets container (one dedicated full-width line per bucket)
        this._detailedBody = createVBox({
            style_class: 'gas-provider-buckets',
            x_expand: true,
        });
        this._fullContainer = createVBox({
            style_class: 'gas-bucket-group-box',
            x_expand: true,
        });
        this._extraInfoLabel = new St.Label({
            text: '',
            style_class: 'gas-extra-info-note',
            visible: false,
        });
        this._extraInfoLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;
        this._statusNoteLabel = new St.Label({
            text: '',
            style_class: 'gas-status-note',
            visible: false,
        });
        this._statusNoteLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        this._detailedBody.add_child(this._fullContainer);
        this._detailedBody.add_child(this._extraInfoLabel);
        this._detailedBody.add_child(this._statusNoteLabel);

        this._middleCol = createVBox({
            style_class: 'gas-provider-mid-col',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._middleCol.add_child(this._titleRow);
        this._middleCol.add_child(this._detailedBody);

        // Compact mode inline metrics (hidden in Detailed mode)
        this._compactRightBox = createHBox({
            style_class: 'gas-compact-metrics',
            y_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });
        this._compactBar = new CapsuleBar({
            width: 56,
            height: 5,
            isMenuOpen,
        });
        this._compactPctLabel = new St.Label({
            text: '0%',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-bucket-pct gas-compact-pct',
        });
        this._compactResetLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-compact-reset',
        });
        this._compactRightBox.add_child(this._compactBar.actor);
        this._compactRightBox.add_child(this._compactPctLabel);
        this._compactRightBox.add_child(this._compactResetLabel);

        this._chevron = new St.Label({
            text: '›',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-chevron',
        });

        this._innerRow = createHBox({
            style_class: 'gas-provider-row-inner',
            x_expand: true,
        });
        this._innerRow.add_child(this._avatar.actor);
        this._innerRow.add_child(this._middleCol);
        this._innerRow.add_child(this._compactRightBox);
        this._innerRow.add_child(this._chevron);

        this.actor = new St.Button({
            style_class: 'gas-provider-card',
            can_focus: true,
            track_hover: true,
            x_expand: true,
            child: this._innerRow,
        });

        connectInteractiveButton(this.actor, () => {
            onSelectProvider(this.providerId);
        });
    }

    _ensureFullRow(index) {
        while (this._fullRowPool.length <= index) {
            const row = createHBox({
                style_class: 'gas-bucket-full-row',
                x_expand: true,
                y_align: Clutter.ActorAlign.CENTER,
            });
            const titleLabel = new St.Label({
                text: '',
                x_expand: true,
                x_align: Clutter.ActorAlign.START,
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-bucket-full-label',
            });
            titleLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

            const bar = new CapsuleBar({
                width: 58,
                height: 5,
                isMenuOpen: this._isMenuOpen,
            });
            const pctLabel = new St.Label({
                text: '0%',
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-bucket-pct gas-bucket-pct-right',
            });

            row.add_child(titleLabel);
            row.add_child(bar.actor);
            row.add_child(pctLabel);
            this._fullContainer.add_child(row);
            this._fullRowPool.push({row, titleLabel, bar, pctLabel});
        }
        return this._fullRowPool[index];
    }

    update(provider, {densityMode, metricMode, themeId, nowEpoch}) {
        const isCompact = densityMode === 'compact';
        this._avatar.setGIcon(this._getIcon(provider.icon_name));
        this._avatar.setCompact(isCompact);

        if (isCompact)
            this.actor.add_style_class_name('is-compact');
        else
            this.actor.remove_style_class_name('is-compact');

        this._nameLabel.set_text(provider.display_name || provider.id);

        const activeAcc = getActiveAccount(provider);
        const badgeText = provider.plan_badge || activeAcc?.plan_tier || '';
        if (badgeText) {
            this._badgeLabel.set_text(`· ${badgeText}`);
            this._badgeLabel.show();
        } else {
            this._badgeLabel.hide();
        }

        const primaryResetEpoch = provider.primary_reset_epoch ??
            activeAcc?.buckets?.find(b => b.is_primary)?.resets_at_epoch ??
            activeAcc?.buckets?.[0]?.resets_at_epoch ??
            null;

        if (isCompact) {
            this._detailedResetLabel.hide();
            this._detailedBody.hide();
            this._compactRightBox.show();

            const primaryBucket = activeAcc?.buckets?.find(b => b.is_primary) ??
                activeAcc?.buckets?.[0] ??
                (typeof provider.primary_used_percent === 'number'
                    ? {
                        used_percent: provider.primary_used_percent,
                        remaining_percent: provider.primary_remaining_percent,
                    }
                    : null);

            if (primaryBucket) {
                const metric = formatBucketMetric(primaryBucket, metricMode);
                this._compactBar.actor.show();
                this._compactBar.update({
                    fillFraction: metric.fillFraction,
                    severity: metric.severity,
                    themeId,
                });
                this._compactPctLabel.set_text(metric.percentText);
                applySeverityClass(this._compactPctLabel, metric.severity);
            } else {
                this._compactBar.actor.hide();
                const fallbackText = activeAcc?.state === 'signed_out' ? 'Sign in' : '—';
                this._compactPctLabel.set_text(fallbackText);
                applySeverityClass(
                    this._compactPctLabel,
                    activeAcc?.state === 'signed_out' ? 'warning' : 'normal'
                );
            }

            const shortReset = formatResetCountdown(primaryResetEpoch, nowEpoch, {prefix: false});
            this._compactResetLabel.set_text(shortReset);
            this._compactResetLabel.visible = Boolean(shortReset);
            return;
        }

        // Detailed mode
        this._compactRightBox.hide();
        this._detailedBody.show();

        const fullReset = formatResetCountdown(primaryResetEpoch, nowEpoch, {prefix: true});
        if (fullReset) {
            this._detailedResetLabel.set_text(fullReset);
            this._detailedResetLabel.show();
        } else {
            this._detailedResetLabel.hide();
        }

        const buckets = Array.isArray(activeAcc?.buckets) ? activeAcc.buckets : [];
        const groups = groupBucketsForDetailedView(buckets);

        let fullIdx = 0;

        for (const group of groups) {
            const items = group.kind === 'pair' ? group.items : [group.item];
            for (const item of items) {
                if (!item)
                    continue;
                const slot = this._ensureFullRow(fullIdx++);
                const metric = formatBucketMetric(item, metricMode);
                slot.titleLabel.set_text(formatBucketDisplayLabel(item));
                slot.bar.update({
                    fillFraction: metric.fillFraction,
                    severity: metric.severity,
                    themeId,
                });
                slot.pctLabel.set_text(metric.percentText);
                applySeverityClass(slot.pctLabel, metric.severity);
                slot.row.show();
            }
        }

        for (let i = fullIdx; i < this._fullRowPool.length; i++)
            this._fullRowPool[i].row.hide();

        this._fullContainer.visible = fullIdx > 0;

        const extraNote = formatCompactAccountExtraNote(activeAcc, Boolean(badgeText));
        if (extraNote) {
            this._extraInfoLabel.set_text(extraNote);
            this._extraInfoLabel.show();
        } else {
            this._extraInfoLabel.hide();
        }

        if (buckets.length === 0) {
            const note = activeAcc?.error ||
                activeAcc?.status_note ||
                (activeAcc?.state === 'signed_out'
                    ? 'Signed out — click to sign in'
                    : `Updates when ${provider.display_name || 'agent'} runs`);
            this._statusNoteLabel.set_text(note);
            this._statusNoteLabel.show();
        } else if (activeAcc?.error) {
            this._statusNoteLabel.set_text(activeAcc.error);
            this._statusNoteLabel.show();
        } else {
            this._statusNoteLabel.hide();
        }
    }

    repaintBars() {
        this._compactBar.repaintIfOpen();
        for (const fullSlot of this._fullRowPool) {
            if (fullSlot.row.visible)
                fullSlot.bar.repaintIfOpen();
        }
    }

    destroy() {
        this._compactBar.destroy();
        for (const fullSlot of this._fullRowPool)
            fullSlot.bar.destroy();
    }
}

/**
 * Page 1 (`Usage · all agents`):
 * - Header (`Usage`, `all agents`, `⚙` Quick Options, `↻` Refresh)
 * - `Detailed` | `Compact` segmented toggle
 * - `St.ScrollView` (`max-height: 516px`, `overlay_scrollbars: true`) of reusable provider cards
 * - Footer (`Manage Accounts & Preferences…`, `Open Config`)
 */
export class MainUsagePage {
    constructor({
        settings,
        getIcon,
        isMenuOpen,
        onRefresh,
        onOpenOptions,
        onSelectProvider,
        onOpenPreferences,
        onOpenConfig,
    }) {
        this._settings = settings;
        this._getIcon = getIcon;
        this._isMenuOpen = isMenuOpen;
        this._onSelectProvider = onSelectProvider;

        this._cardMap = new Map();

        this.actor = createVBox({
            style_class: 'gas-page gas-main-page',
            x_expand: true,
        });

        // Header
        const headerSection = createVBox({
            style_class: 'gas-header-section',
            x_expand: true,
        });

        const topBarRow = createHBox({
            style_class: 'gas-header-top-row',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });

        const titleLabel = new St.Label({
            text: 'Usage',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-header-title',
        });

        this._subtitleLabel = new St.Label({
            text: 'all agents',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-header-subtitle',
        });

        this._optionsBtn = new St.Button({
            style_class: 'gas-icon-btn',
            can_focus: true,
            track_hover: true,
            accessible_name: 'Quick Options',
            child: new St.Label({
                text: '⚙',
                x_align: Clutter.ActorAlign.CENTER,
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-icon-btn-glyph',
            }),
        });
        connectInteractiveButton(this._optionsBtn, () => {
            onOpenOptions();
        });

        this._refreshGlyph = new St.Label({
            text: '↻',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-icon-btn-glyph',
        });
        this._refreshBtn = new St.Button({
            style_class: 'gas-icon-btn',
            can_focus: true,
            track_hover: true,
            accessible_name: 'Refresh Usage',
            child: this._refreshGlyph,
        });
        connectInteractiveButton(this._refreshBtn, () => {
            onRefresh();
        });

        topBarRow.add_child(titleLabel);
        topBarRow.add_child(this._subtitleLabel);
        topBarRow.add_child(this._optionsBtn);
        topBarRow.add_child(this._refreshBtn);

        this._densityToggle = new SegmentedToggle({
            options: DENSITY_MODES,
            activeId: this._settings.get_string('density-mode') || 'detailed',
            onChange: mode => {
                this._settings.set_string('density-mode', mode);
            },
        });

        headerSection.add_child(topBarRow);
        headerSection.add_child(this._densityToggle.actor);

        // Scrollable provider cards viewport
        this._scrollView = new St.ScrollView({
            style_class: 'gas-scroll-view',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: true,
            x_expand: true,
        });

        this._cardsBox = createVBox({
            style_class: 'gas-providers-list',
            x_expand: true,
        });

        this._errorBanner = new St.Label({
            text: '',
            style_class: 'gas-error-banner',
            visible: false,
        });
        this._errorBanner.clutter_text.line_wrap = true;

        this._emptyBox = createVBox({
            style_class: 'gas-empty-state',
            x_expand: true,
            visible: false,
        });
        this._emptyTitle = new St.Label({
            text: 'No active AI agents detected',
            style_class: 'gas-empty-title',
            x_align: Clutter.ActorAlign.CENTER,
        });
        this._emptyDetail = new St.Label({
            text: 'Sign into Claude, Codex, Grok, Antigravity, or Cursor — or configure custom locations in Preferences.',
            style_class: 'gas-empty-detail',
            x_align: Clutter.ActorAlign.CENTER,
        });
        this._emptyDetail.clutter_text.line_wrap = true;
        this._emptyBox.add_child(this._emptyTitle);
        this._emptyBox.add_child(this._emptyDetail);

        this._cardsListInner = createVBox({
            style_class: 'gas-cards-inner',
            x_expand: true,
        });

        this._cardsBox.add_child(this._errorBanner);
        this._cardsBox.add_child(this._emptyBox);
        this._cardsBox.add_child(this._cardsListInner);
        setScrollChild(this._scrollView, this._cardsBox);

        // Footer
        const footerBox = createVBox({
            style_class: 'gas-footer-section',
            x_expand: true,
        });

        const prefsRow = createFooterRowButton({
            title: 'Manage Accounts & Preferences…',
            onActivate: () => onOpenPreferences(),
        });
        const configRow = createFooterRowButton({
            title: 'Open Config',
            onActivate: () => onOpenConfig(),
        });
        footerBox.add_child(prefsRow.actor);
        footerBox.add_child(configRow.actor);

        this.actor.add_child(headerSection);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(this._scrollView);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(footerBox);
    }

    setLoading(isLoading) {
        if (isLoading) {
            this._refreshBtn.add_style_class_name('is-loading');
            this._refreshBtn.opacity = 140;
            this._subtitleLabel.set_text('updating…');
        } else {
            this._refreshBtn.remove_style_class_name('is-loading');
            this._refreshBtn.opacity = 255;
            this._subtitleLabel.set_text('all agents');
        }
    }

    update({visibleProviders, errorMessage, densityMode, metricMode, themeId, nowEpoch}) {
        this._densityToggle.setActive(densityMode);

        if (errorMessage) {
            this._errorBanner.set_text(errorMessage);
            this._errorBanner.show();
        } else {
            this._errorBanner.hide();
        }

        const providers = Array.isArray(visibleProviders) ? visibleProviders : [];
        if (providers.length === 0) {
            this._emptyBox.show();
            for (const card of this._cardMap.values())
                card.actor.hide();
            return;
        }

        this._emptyBox.hide();
        const seenIds = new Set();

        for (let idx = 0; idx < providers.length; idx++) {
            const prov = providers[idx];
            seenIds.add(prov.id);

            let card = this._cardMap.get(prov.id);
            if (!card) {
                card = new ProviderCardWidget({
                    providerId: prov.id,
                    getIcon: this._getIcon,
                    isMenuOpen: this._isMenuOpen,
                    onSelectProvider: this._onSelectProvider,
                });
                this._cardMap.set(prov.id, card);
                this._cardsListInner.add_child(card.actor);
            }

            const currentAtIndex = this._cardsListInner.get_child_at_index(idx);
            if (currentAtIndex !== card.actor)
                this._cardsListInner.set_child_at_index(card.actor, idx);

            card.actor.show();
            card.update(prov, {densityMode, metricMode, themeId, nowEpoch});
        }

        for (const [id, card] of this._cardMap.entries()) {
            if (!seenIds.has(id))
                card.actor.hide();
        }
    }

    repaintBars() {
        for (const card of this._cardMap.values()) {
            if (card.actor.visible)
                card.repaintBars();
        }
    }

    destroy() {
        for (const card of this._cardMap.values())
            card.destroy();
        this._cardMap.clear();
    }
}
