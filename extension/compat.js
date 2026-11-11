import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import St from 'gi://St';

const SUPPORTS_ORIENTATION = Boolean(St.BoxLayout.find_property?.('orientation'));

/**
 * Attaches a single child actor to an St.ScrollView across GNOME 45-50.
 * GNOME 46+ removed Clutter.Container.add_actor() in favor of set_child().
 *
 * @param {St.ScrollView} scrollView
 * @param {Clutter.Actor} child
 */
export function setScrollChild(scrollView, child) {
    if (typeof scrollView.set_child === 'function')
        scrollView.set_child(child);
    else
        scrollView.add_actor(child);
}

/**
 * Builds constructor properties for a vertical St.BoxLayout across GNOME 45-50.
 * GNOME 48+ deprecated `vertical: true` in favor of `orientation`.
 *
 * @param {object} extra
 * @returns {object}
 */
export function vBoxParams(extra = {}) {
    return SUPPORTS_ORIENTATION
        ? {orientation: Clutter.Orientation.VERTICAL, ...extra}
        : {vertical: true, ...extra};
}

/**
 * Builds constructor properties for a horizontal St.BoxLayout across GNOME 45-50.
 *
 * @param {object} extra
 * @returns {object}
 */
export function hBoxParams(extra = {}) {
    return SUPPORTS_ORIENTATION
        ? {orientation: Clutter.Orientation.HORIZONTAL, ...extra}
        : {vertical: false, ...extra};
}

/**
 * Creates a vertical St.BoxLayout using version-safe orientation properties.
 *
 * @param {object} extra
 * @returns {St.BoxLayout}
 */
export function createVBox(extra = {}) {
    return new St.BoxLayout(vBoxParams(extra));
}

/**
 * Creates a horizontal St.BoxLayout using version-safe orientation properties.
 *
 * @param {object} extra
 * @returns {St.BoxLayout}
 */
export function createHBox(extra = {}) {
    return new St.BoxLayout(hBoxParams(extra));
}

/**
 * Opens org.gnome.desktop.interface settings safely to query reduced-motion preferences.
 *
 * @returns {Gio.Settings|null}
 */
export function createDesktopInterfaceSettings() {
    try {
        return new Gio.Settings({schema_id: 'org.gnome.desktop.interface'});
    } catch (_e) {
        return null;
    }
}

/**
 * Returns whether desktop animations are enabled.
 *
 * @param {Gio.Settings|null} desktopSettings
 * @returns {boolean}
 */
export function areAnimationsEnabled(desktopSettings) {
    if (!desktopSettings)
        return true;
    try {
        return desktopSettings.get_boolean('enable-animations');
    } catch (_e) {
        return true;
    }
}

/**
 * Returns 130ms opacity crossfade duration when animations are enabled, or 0ms otherwise.
 * Never uses scale transforms.
 *
 * @param {Gio.Settings|null} desktopSettings
 * @returns {number}
 */
export function getCrossfadeDurationMs(desktopSettings) {
    return areAnimationsEnabled(desktopSettings) ? 130 : 0;
}

/**
 * Transitions from outgoingActor to incomingActor using an opacity-only crossfade
 * (130ms when animations are enabled, 0ms instant swap when disabled).
 *
 * @param {Clutter.Actor|null} outgoingActor
 * @param {Clutter.Actor} incomingActor
 * @param {Gio.Settings|null} desktopSettings
 * @param {Function} [onComplete]
 */
export function crossfadePages(outgoingActor, incomingActor, desktopSettings, onComplete) {
    if (outgoingActor && outgoingActor !== incomingActor) {
        outgoingActor.remove_all_transitions?.();
        outgoingActor.hide();
        outgoingActor.opacity = 255;
    }

    incomingActor.remove_all_transitions?.();
    const duration = getCrossfadeDurationMs(desktopSettings);

    if (duration <= 0 || typeof incomingActor.ease !== 'function') {
        incomingActor.opacity = 255;
        incomingActor.show();
        onComplete?.();
        return;
    }

    incomingActor.opacity = 0;
    incomingActor.show();
    incomingActor.ease({
        opacity: 255,
        duration,
        mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        onComplete: () => {
            onComplete?.();
        },
    });
}

/**
 * Wires an interactive St.Button inside a PopupMenu so mouse/touch/keyboard clicks
 * invoke `callback` cleanly across GNOME 45-50 without blocking Clutter.ClickGesture.
 *
 * @param {St.Button} button
 * @param {Function} callback
 * @returns {number[]} Connected signal IDs on `button`
 */
export function connectInteractiveButton(button, callback) {
    return [
        button.connect('clicked', () => {
            callback();
        }),
    ];
}

/**
 * Marks a custom popover root container reactive so PopupMenuManager treats
 * non-interactive background clicks inside the container as inside the menu
 * without returning Clutter.EVENT_STOP on legacy button signals (which cancels
 * St.Button Clutter.ClickGesture recognition in GNOME Shell 49/50).
 *
 * @param {Clutter.Actor} container
 * @returns {number[]} Connected signal IDs on `container`
 */
export function containPopoverEvents(container) {
    container.reactive = true;
    return [];
}
