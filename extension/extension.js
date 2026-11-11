import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {BackendClient} from './backend.js';
import {AgentStatusIndicator} from './indicator.js';
import {PopoverController} from './popover/controller.js';

const VALID_PANEL_BOXES = new Set(['left', 'center', 'right']);

export default class AgentStatusExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._placementSigIds = [];

        this._backend = new BackendClient({
            settings: this._settings,
        });

        this._indicator = new AgentStatusIndicator({
            extension: this,
            settings: this._settings,
            backend: this._backend,
        });

        const popoverController = new PopoverController({
            indicator: this._indicator,
            settings: this._settings,
            backend: this._backend,
        });
        this._indicator.attachPopoverController(popoverController);

        this._backend.setCallbacks({
            onReport: report => this._indicator?.handleReport(report),
            onError: message => this._indicator?.handleError(message),
            onLoadingChanged: loading => this._indicator?.handleLoadingChanged(loading),
        });

        const initialBox = this._getPanelBoxName();
        const initialPos = this._settings.get_int('panel-position');
        Main.panel.addToStatusArea(this.uuid, this._indicator, initialPos, initialBox);

        this._placementSigIds.push(
            this._settings.connect('changed::panel-box', () => this._repositionIndicator()),
            this._settings.connect('changed::panel-position', () => this._repositionIndicator())
        );

        this._backend.start();
    }

    _getPanelBoxName() {
        const raw = this._settings?.get_string('panel-box') || 'right';
        return VALID_PANEL_BOXES.has(raw) ? raw : 'right';
    }

    _getTargetPanelBoxActor(boxName) {
        if (boxName === 'left')
            return Main.panel._leftBox;
        if (boxName === 'center')
            return Main.panel._centerBox;
        return Main.panel._rightBox;
    }

    _repositionIndicator() {
        if (!this._indicator || !this._settings)
            return;

        const boxName = this._getPanelBoxName();
        const pos = this._settings.get_int('panel-position');
        const targetBox = this._getTargetPanelBoxActor(boxName);
        const container = this._indicator.container;

        if (!targetBox || !container)
            return;

        const currentParent = container.get_parent();
        if (currentParent)
            currentParent.remove_child(container);

        const maxIdx = targetBox.get_n_children();
        const clampedPos = Math.max(0, Math.min(pos, maxIdx));
        targetBox.insert_child_at_index(container, clampedPos);
    }

    disable() {
        if (this._settings && this._placementSigIds) {
            for (const sigId of this._placementSigIds)
                this._settings.disconnect(sigId);
        }
        this._placementSigIds = [];

        if (this._backend) {
            this._backend.destroy();
            this._backend = null;
        }

        if (this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }

        this._settings = null;
    }
}
