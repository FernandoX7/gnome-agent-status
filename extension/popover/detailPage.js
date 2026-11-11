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
    computeBucketPace,
    formatBucketDisplayLabel,
    formatBucketMetric,
    formatCreditsSummary,
    formatRelativeUpdated,
    formatResetCountdown,
    getActiveAccount,
} from '../format.js';
import {
    CapsuleBar,
    applySeverityClass,
    createFooterRowButton,
} from './widgets.js';

/**
 * Reusable multi-account comparison row inside the Provider Drill-Down switcher table.
 */
class AccountSwitcherRow {
    constructor({isMenuOpen, onActivate}) {
        this._isMenuOpen = isMenuOpen;
        this._accountId = '';
        this._miniBucketPool = [];

        this._checkLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-account-check',
        });

        this._nameLabel = new St.Label({
            text: '',
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-account-name',
        });
        this._nameLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        this._sourceLabel = new St.Label({
            text: '',
            x_align: Clutter.ActorAlign.START,
            style_class: 'gas-account-source',
            visible: false,
        });
        this._sourceLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        this._identityCol = createVBox({
            style_class: 'gas-account-id-col',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._identityCol.add_child(this._nameLabel);
        this._identityCol.add_child(this._sourceLabel);

        this._miniBarsCol = createVBox({
            style_class: 'gas-account-minibars-col',
            y_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });

        this._passiveNoteLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-account-passive-note',
            visible: false,
        });
        this._passiveNoteLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        const inner = createHBox({
            style_class: 'gas-account-row-inner',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        inner.add_child(this._checkLabel);
        inner.add_child(this._identityCol);
        inner.add_child(this._miniBarsCol);
        inner.add_child(this._passiveNoteLabel);

        this.actor = new St.Button({
            style_class: 'gas-account-row',
            can_focus: true,
            track_hover: true,
            x_expand: true,
            child: inner,
        });

        connectInteractiveButton(this.actor, () => {
            if (this._accountId)
                onActivate(this._accountId);
        });
    }

    _ensureMiniBucketSlot(index) {
        while (this._miniBucketPool.length <= index) {
            const row = createHBox({
                style_class: 'gas-account-minibar-line',
                y_align: Clutter.ActorAlign.CENTER,
            });
            const shortLabel = new St.Label({
                text: '',
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-account-minibar-tag',
            });
            const bar = new CapsuleBar({
                width: 38,
                height: 4,
                isMenuOpen: this._isMenuOpen,
            });
            const pctLabel = new St.Label({
                text: '0%',
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-account-minibar-pct',
            });
            const resetLabel = new St.Label({
                text: '',
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'gas-account-minibar-reset',
            });

            row.add_child(shortLabel);
            row.add_child(bar.actor);
            row.add_child(pctLabel);
            row.add_child(resetLabel);
            this._miniBarsCol.add_child(row);
            this._miniBucketPool.push({row, shortLabel, bar, pctLabel, resetLabel});
        }
        return this._miniBucketPool[index];
    }

    update({
        account,
        providerDisplayName,
        isSelected,
        showSourceDisambiguator,
        metricMode,
        themeId,
        nowEpoch,
    }) {
        this._accountId = account.id;
        this._checkLabel.set_text(isSelected ? '✓' : '');

        if (isSelected)
            this.actor.add_style_class_name('is-selected');
        else
            this.actor.remove_style_class_name('is-selected');

        this._nameLabel.set_text(account.label || account.id);
        if (showSourceDisambiguator && account.source_label) {
            this._sourceLabel.set_text(account.source_label);
            this._sourceLabel.show();
        } else {
            this._sourceLabel.hide();
        }

        const buckets = Array.isArray(account.buckets) ? account.buckets : [];

        // Selected active account already renders full-width gauges in the Hero Card above
        if (isSelected) {
            this._miniBarsCol.hide();
            this._passiveNoteLabel.hide();
            return;
        }

        if (buckets.length > 0 && account.state !== 'passive_cached') {
            this._passiveNoteLabel.hide();
            this._miniBarsCol.show();
            const shownBuckets = buckets.slice(0, 3);
            for (let i = 0; i < shownBuckets.length; i++) {
                const b = shownBuckets[i];
                const slot = this._ensureMiniBucketSlot(i);
                const metric = formatBucketMetric(b, metricMode);
                slot.shortLabel.set_text(b.short_label || 'wk');
                slot.bar.update({
                    fillFraction: metric.fillFraction,
                    severity: metric.severity,
                    themeId,
                });
                slot.pctLabel.set_text(metric.percentText);
                applySeverityClass(slot.pctLabel, metric.severity);
                slot.resetLabel.set_text(
                    formatResetCountdown(b.resets_at_epoch, nowEpoch, {prefix: false})
                );
                slot.row.show();
            }
            for (let i = shownBuckets.length; i < this._miniBucketPool.length; i++)
                this._miniBucketPool[i].row.hide();
            return;
        }

        this._miniBarsCol.hide();
        const passiveText = account.status_note ||
            account.error ||
            `Updates when ${providerDisplayName} runs`;
        this._passiveNoteLabel.set_text(passiveText);
        this._passiveNoteLabel.show();
    }

    repaintBars() {
        if (!this._miniBarsCol.visible)
            return;
        for (const slot of this._miniBucketPool) {
            if (slot.row.visible)
                slot.bar.repaintIfOpen();
        }
    }

    destroy() {
        for (const slot of this._miniBucketPool)
            slot.bar.destroy();
        this._miniBucketPool = [];
    }
}

/**
 * Page 2 (`Provider Drill-Down`):
 * - `‹` Back nav + brand icon + provider name + `Updated just now` / relative age
 * - Recessed Active Account Hero Card (full-width `6px` capsule bars + `% used`/`% left` + `Resets in ...` + `credits`)
 * - Multi-Account Switcher Table (`System default` + accounts with `✓` active pill, mini-bars, passive state)
 * - `Signed out` section with inline `[Sign in]` terminal button
 */
export class ProviderDetailPage {
    constructor({
        getIcon,
        isMenuOpen,
        tooltips = null,
        onBack,
        onSelectAccount,
        onSignIn,
        onOpenDashboard,
        onOpenPreferences,
    }) {
        this._getIcon = getIcon;
        this._isMenuOpen = isMenuOpen;
        this._tooltips = tooltips;
        this._onSelectAccount = onSelectAccount;
        this._onSignIn = onSignIn;
        this._onOpenDashboard = onOpenDashboard;

        this._currentProviderId = null;
        this._currentDashboardUrl = null;
        this._pinnedDefaultIds = new Set();

        this._heroBucketPool = [];
        this._accountRowPool = [];
        this._signedOutRowPool = [];

        this.actor = createVBox({
            style_class: 'gas-page gas-detail-page',
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
        const backChevron = new St.Label({
            text: '‹',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-back-chevron',
        });
        this._headerBrandIcon = new St.Icon({
            icon_size: 16,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-brand-icon',
        });
        this._headerTitleLabel = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-detail-title',
        });
        backInner.add_child(backChevron);
        backInner.add_child(this._headerBrandIcon);
        backInner.add_child(this._headerTitleLabel);

        this._backBtn = new St.Button({
            style_class: 'gas-back-btn',
            can_focus: true,
            track_hover: true,
            accessible_name: 'Back to All Agents (Esc)',
            child: backInner,
        });
        connectInteractiveButton(this._backBtn, () => {
            onBack();
        });
        this._tooltips?.attach(this._backBtn, 'Back to All Agents (Esc)');

        const headerSpacer = new St.Widget({x_expand: true});

        this._updatedLabel = new St.Label({
            text: 'Updated just now',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-updated-label',
        });

        headerRow.add_child(this._backBtn);
        headerRow.add_child(headerSpacer);
        headerRow.add_child(this._updatedLabel);

        // Scrollable drill-down body
        this._scrollView = new St.ScrollView({
            style_class: 'gas-scroll-view gas-detail-scroll',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: true,
            x_expand: true,
        });

        const bodyBox = createVBox({
            style_class: 'gas-detail-body',
            x_expand: true,
        });

        // 1. Active Account Hero Card
        this._heroCard = createVBox({
            style_class: 'gas-hero-card',
            x_expand: true,
        });

        const heroHeaderRow = createHBox({
            style_class: 'gas-hero-header-row',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._heroAccountLabel = new St.Label({
            text: '',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-hero-account-title',
        });
        this._heroAccountLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        this._heroTierBadge = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-hero-tier-badge',
            visible: false,
        });
        heroHeaderRow.add_child(this._heroAccountLabel);
        heroHeaderRow.add_child(this._heroTierBadge);

        this._heroBucketsBox = createVBox({
            style_class: 'gas-hero-buckets-box',
            x_expand: true,
        });

        this._heroEmptyNote = new St.Label({
            text: '',
            style_class: 'gas-hero-empty-note',
            visible: false,
        });
        this._heroEmptyNote.clutter_text.line_wrap = true;

        this._heroCreditsLabel = new St.Label({
            text: '',
            style_class: 'gas-hero-credits',
            visible: false,
        });
        this._heroCreditsLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

        this._heroCard.add_child(heroHeaderRow);
        this._heroCard.add_child(this._heroBucketsBox);
        this._heroCard.add_child(this._heroEmptyNote);
        this._heroCard.add_child(this._heroCreditsLabel);

        // 2. Multi-Account Switcher Table
        this._accountsSection = createVBox({
            style_class: 'gas-accounts-section',
            x_expand: true,
        });

        const colHeaderRow = createHBox({
            style_class: 'gas-accounts-col-headers',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._colAccountHeader = new St.Label({
            text: 'Account',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            style_class: 'gas-col-header-label',
        });
        this._colMetricHeader = new St.Label({
            text: 'Used',
            style_class: 'gas-col-header-label gas-col-header-used',
        });
        this._colResetHeader = new St.Label({
            text: 'Reset',
            style_class: 'gas-col-header-label gas-col-header-reset',
        });
        colHeaderRow.add_child(this._colAccountHeader);
        colHeaderRow.add_child(this._colMetricHeader);
        colHeaderRow.add_child(this._colResetHeader);

        // `System default` row
        this._sysDefaultCheck = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-account-check',
        });
        const sysDefaultTitle = new St.Label({
            text: 'System default',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'gas-account-name',
        });
        const sysDefaultInner = createHBox({
            style_class: 'gas-account-row-inner',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        sysDefaultInner.add_child(this._sysDefaultCheck);
        sysDefaultInner.add_child(sysDefaultTitle);

        this._sysDefaultBtn = new St.Button({
            style_class: 'gas-account-row',
            can_focus: true,
            track_hover: true,
            x_expand: true,
            child: sysDefaultInner,
        });
        connectInteractiveButton(this._sysDefaultBtn, () => {
            if (!this._currentProviderId)
                return;
            this._pinnedDefaultIds.add(this._currentProviderId);
            this._onSelectAccount(this._currentProviderId, 'default');
        });

        this._accountRowsBox = createVBox({
            style_class: 'gas-account-rows-box',
            x_expand: true,
        });

        this._accountsSection.add_child(colHeaderRow);
        this._accountsSection.add_child(this._sysDefaultBtn);
        this._accountsSection.add_child(this._accountRowsBox);

        // 3. `Signed out` Section
        this._signedOutSection = createVBox({
            style_class: 'gas-signed-out-section',
            x_expand: true,
            visible: false,
        });
        const signedOutHeader = new St.Label({
            text: 'Signed out',
            x_align: Clutter.ActorAlign.START,
            style_class: 'gas-section-header',
        });
        this._signedOutList = createVBox({
            style_class: 'gas-signed-out-list',
            x_expand: true,
        });
        this._signedOutSection.add_child(signedOutHeader);
        this._signedOutSection.add_child(this._signedOutList);

        bodyBox.add_child(this._heroCard);
        bodyBox.add_child(this._accountsSection);
        bodyBox.add_child(this._signedOutSection);
        setScrollChild(this._scrollView, bodyBox);

        // Footer
        const footerBox = createVBox({
            style_class: 'gas-footer-section',
            x_expand: true,
        });
        this._dashboardRow = createFooterRowButton({
            title: 'Usage Dashboard…',
            trailingGlyph: '↗',
            onActivate: () => {
                if (this._currentDashboardUrl)
                    this._onOpenDashboard(this._currentDashboardUrl);
            },
        });
        const manageRow = createFooterRowButton({
            title: 'Manage Accounts…',
            onActivate: () => onOpenPreferences(),
        });
        footerBox.add_child(this._dashboardRow.actor);
        footerBox.add_child(manageRow.actor);

        this.actor.add_child(headerRow);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(this._scrollView);
        this.actor.add_child(new St.Widget({style_class: 'gas-divider', x_expand: true}));
        this.actor.add_child(footerBox);
    }

    _ensureHeroBucketSlot(index) {
        while (this._heroBucketPool.length <= index) {
            const block = createVBox({
                style_class: 'gas-hero-bucket-block',
                x_expand: true,
            });
            const titleLabel = new St.Label({
                text: '',
                x_align: Clutter.ActorAlign.START,
                style_class: 'gas-hero-bucket-title',
            });
            const bar = new CapsuleBar({
                height: 6,
                xExpand: true,
                isHero: true,
                isMenuOpen: this._isMenuOpen,
            });
            const captionRow = createHBox({
                style_class: 'gas-hero-caption-row',
                x_expand: true,
                y_align: Clutter.ActorAlign.CENTER,
            });
            const usedCaption = new St.Label({
                text: '',
                x_expand: true,
                x_align: Clutter.ActorAlign.START,
                style_class: 'gas-hero-caption-left',
            });
            const resetCaption = new St.Label({
                text: '',
                x_align: Clutter.ActorAlign.END,
                style_class: 'gas-hero-caption-right',
            });
            captionRow.add_child(usedCaption);
            captionRow.add_child(resetCaption);

            const paceLabel = new St.Label({
                text: '',
                x_align: Clutter.ActorAlign.START,
                style_class: 'gas-hero-pace-pill',
                visible: false,
            });
            paceLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

            block.add_child(titleLabel);
            block.add_child(bar.actor);
            block.add_child(captionRow);
            block.add_child(paceLabel);
            this._heroBucketsBox.add_child(block);
            this._heroBucketPool.push({
                block,
                titleLabel,
                bar,
                usedCaption,
                resetCaption,
                paceLabel,
            });
        }
        return this._heroBucketPool[index];
    }

    _ensureAccountRowSlot(index) {
        while (this._accountRowPool.length <= index) {
            const rowWidget = new AccountSwitcherRow({
                isMenuOpen: this._isMenuOpen,
                onActivate: accountId => {
                    if (!this._currentProviderId)
                        return;
                    this._pinnedDefaultIds.delete(this._currentProviderId);
                    this._onSelectAccount(this._currentProviderId, accountId);
                },
            });
            this._accountRowsBox.add_child(rowWidget.actor);
            this._accountRowPool.push(rowWidget);
        }
        return this._accountRowPool[index];
    }

    _ensureSignedOutSlot(index) {
        while (this._signedOutRowPool.length <= index) {
            const row = createHBox({
                style_class: 'gas-signed-out-row',
                x_expand: true,
                y_align: Clutter.ActorAlign.CENTER,
            });
            const leftCol = createVBox({
                style_class: 'gas-signed-out-left',
                x_expand: true,
            });
            const nameLabel = new St.Label({
                text: '',
                x_align: Clutter.ActorAlign.START,
                style_class: 'gas-signed-out-name',
            });
            nameLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

            const hintLabel = new St.Label({
                text: 'Sign in again to use this account',
                x_align: Clutter.ActorAlign.START,
                style_class: 'gas-signed-out-hint',
            });
            hintLabel.clutter_text.ellipsize = Pango.EllipsizeMode.END;

            leftCol.add_child(nameLabel);
            leftCol.add_child(hintLabel);

            const slotObj = {
                row,
                nameLabel,
                hintLabel,
                loginCommand: '',
            };

            const signInBtn = new St.Button({
                style_class: 'gas-signin-btn',
                can_focus: true,
                track_hover: true,
                y_align: Clutter.ActorAlign.CENTER,
                child: new St.Label({
                    text: 'Sign in',
                    y_align: Clutter.ActorAlign.CENTER,
                    style_class: 'gas-signin-btn-label',
                }),
            });
            connectInteractiveButton(signInBtn, () => {
                if (slotObj.loginCommand)
                    this._onSignIn(slotObj.loginCommand);
            });
            this._tooltips?.attach(
                signInBtn,
                () => (slotObj.loginCommand ? `Run: ${slotObj.loginCommand}` : 'Sign in via terminal')
            );

            row.add_child(leftCol);
            row.add_child(signInBtn);
            this._signedOutList.add_child(row);
            this._signedOutRowPool.push(slotObj);
        }
        return this._signedOutRowPool[index];
    }

    update({provider, reportUpdatedEpoch, metricMode, themeId, nowEpoch}) {
        if (!provider)
            return;

        this._currentProviderId = provider.id;
        this._currentDashboardUrl = provider.dashboard_url || null;
        this._dashboardRow.actor.visible = Boolean(this._currentDashboardUrl);

        this._headerBrandIcon.set_gicon(this._getIcon(provider.icon_name));
        const displayName = provider.display_name || provider.id;
        this._headerTitleLabel.set_text(displayName);

        const activeAcc = getActiveAccount(provider);
        const updatedEpoch = activeAcc?.updated_at_epoch || reportUpdatedEpoch || 0;
        this._updatedLabel.set_text(formatRelativeUpdated(updatedEpoch, nowEpoch));

        // Hero Card
        this._heroAccountLabel.set_text(activeAcc?.label || 'System default');
        const tierBadge = activeAcc?.plan_tier || provider.plan_badge || activeAcc?.source_label || '';
        if (tierBadge) {
            this._heroTierBadge.set_text(tierBadge);
            this._heroTierBadge.show();
        } else {
            this._heroTierBadge.hide();
        }

        const heroBuckets = Array.isArray(activeAcc?.buckets) ? activeAcc.buckets : [];
        for (let i = 0; i < heroBuckets.length; i++) {
            const b = heroBuckets[i];
            const slot = this._ensureHeroBucketSlot(i);
            const metric = formatBucketMetric(b, metricMode);

            slot.titleLabel.set_text(formatBucketDisplayLabel(b));
            slot.bar.update({
                fillFraction: metric.fillFraction,
                severity: metric.severity,
                themeId,
            });
            slot.usedCaption.set_text(metric.heroCaption);
            slot.resetCaption.set_text(
                formatResetCountdown(b.resets_at_epoch, nowEpoch, {prefix: true})
            );
            const pace = computeBucketPace(b, nowEpoch);
            if (pace) {
                slot.paceLabel.set_text(pace.text);
                applySeverityClass(slot.paceLabel, pace.severity);
                slot.paceLabel.show();
            } else {
                slot.paceLabel.hide();
            }
            slot.block.show();
        }
        for (let i = heroBuckets.length; i < this._heroBucketPool.length; i++)
            this._heroBucketPool[i].block.hide();

        this._heroBucketsBox.visible = heroBuckets.length > 0;

        if (heroBuckets.length === 0) {
            const emptyMsg = activeAcc?.error ||
                activeAcc?.status_note ||
                (activeAcc?.state === 'signed_out'
                    ? 'This account is signed out. Click Sign in below to re-authenticate.'
                    : `Updates when ${displayName} runs`);
            this._heroEmptyNote.set_text(emptyMsg);
            this._heroEmptyNote.show();
        } else {
            this._heroEmptyNote.hide();
        }

        const creditsText = formatCreditsSummary(activeAcc?.credits) ||
            (typeof activeAcc?.extra_info === 'string' ? activeAcc.extra_info.trim() : '');
        if (creditsText) {
            this._heroCreditsLabel.set_text(creditsText);
            this._heroCreditsLabel.show();
        } else {
            this._heroCreditsLabel.hide();
        }

        // Account Switcher Table
        this._colAccountHeader.set_text(`${displayName} Account`);
        this._colMetricHeader.set_text(metricMode === 'remaining' ? 'Left' : 'Used');

        const allAccounts = Array.isArray(provider.accounts) ? provider.accounts : [];
        const usableAccounts = allAccounts.filter(a => a && a.state !== 'signed_out');
        const signedOutAccounts = allAccounts.filter(a => a && a.state === 'signed_out');

        // Detect duplicate labels (e.g. same email across two directories) so source_label disambiguates
        const labelCounts = new Map();
        for (const acc of usableAccounts)
            labelCounts.set(acc.label, (labelCounts.get(acc.label) || 0) + 1);

        const isDefaultPinned = provider.active_account_id === 'default' ||
            this._pinnedDefaultIds.has(provider.id) && activeAcc?.is_system_default;

        this._sysDefaultCheck.set_text(isDefaultPinned ? '✓' : '');
        if (isDefaultPinned)
            this._sysDefaultBtn.add_style_class_name('is-selected');
        else
            this._sysDefaultBtn.remove_style_class_name('is-selected');

        for (let i = 0; i < usableAccounts.length; i++) {
            const acc = usableAccounts[i];
            const rowWidget = this._ensureAccountRowSlot(i);
            const isSelected = !isDefaultPinned && (
                (provider.active_account_id && acc.id === provider.active_account_id) ||
                (!provider.active_account_id && Boolean(acc.is_active))
            );
            rowWidget.actor.show();
            rowWidget.update({
                account: acc,
                providerDisplayName: displayName,
                isSelected,
                showSourceDisambiguator: (labelCounts.get(acc.label) || 0) > 1,
                metricMode,
                themeId,
                nowEpoch,
            });
        }
        for (let i = usableAccounts.length; i < this._accountRowPool.length; i++)
            this._accountRowPool[i].actor.hide();

        // Signed out section
        if (signedOutAccounts.length > 0) {
            this._signedOutSection.show();
            for (let i = 0; i < signedOutAccounts.length; i++) {
                const acc = signedOutAccounts[i];
                const slot = this._ensureSignedOutSlot(i);
                slot.nameLabel.set_text(acc.label || acc.id);
                slot.hintLabel.set_text(
                    acc.status_note || acc.error || 'Sign in again to use this account'
                );
                slot.loginCommand = acc.login_command || '';
                slot.row.show();
            }
            for (let i = signedOutAccounts.length; i < this._signedOutRowPool.length; i++)
                this._signedOutRowPool[i].row.hide();
        } else {
            this._signedOutSection.hide();
        }
    }

    focusBackButton() {
        this._backBtn?.grab_key_focus?.();
    }

    repaintBars() {
        for (const slot of this._heroBucketPool) {
            if (slot.block.visible)
                slot.bar.repaintIfOpen();
        }
        for (const rowWidget of this._accountRowPool) {
            if (rowWidget.actor.visible)
                rowWidget.repaintBars();
        }
    }

    destroy() {
        for (const slot of this._heroBucketPool)
            slot.bar.destroy();
        this._heroBucketPool = [];

        for (const rowWidget of this._accountRowPool)
            rowWidget.destroy();
        this._accountRowPool = [];
    }
}
