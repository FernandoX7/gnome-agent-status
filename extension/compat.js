import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import St from 'gi://St';

const SUPPORTS_ORIENTATION = Boolean(St.BoxLayout.find_property?.('orientation'));
const FALLBACK_STRING_STORE = new WeakMap();

let _gettextFn = s => s;

/**
 * Initializes the active gettext translator function (called from `Extension.enable()`),
 * or resets to the identity function on `Extension.disable()`.
 *
 * @param {((str: string) => string)|null|undefined} fn
 */
export function initGettext(fn) {
    _gettextFn = typeof fn === 'function' ? fn : (s => s);
}

/**
 * Translates a UI string using the extension's gettext domain, falling back safely
 * to the original string when uninitialized or outside GNOME Shell.
 *
 * @param {string} str
 * @returns {string}
 */
export function _(str) {
    if (typeof str !== 'string' || str === '')
        return '';
    try {
        return _gettextFn(str) || str;
    } catch (_e) {
        return str;
    }
}

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
 * Uses opacity transitions only (zero size transforms).
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

/**
 * Checks whether a Gio.Settings instance declares `key` in its compiled schema
 * so reading newly added keys never throws before schema recompilation.
 *
 * @param {Gio.Settings|null|undefined} settings
 * @param {string} key
 * @returns {boolean}
 */
export function hasSettingKey(settings, key) {
    if (!settings || !key)
        return false;
    try {
        const schema = settings.settings_schema;
        if (schema && typeof schema.has_key === 'function')
            return Boolean(schema.has_key(key));
        if (typeof settings.list_keys === 'function')
            return settings.list_keys().includes(key);
        return true;
    } catch (_e) {
        return false;
    }
}

/**
 * Reads a string (`s`) GSettings key safely, returning `fallback` (or an in-memory
 * fallback value if set via `setSettingStringSafe`) when the key is absent from
 * an older compiled schema.
 *
 * @param {Gio.Settings|null|undefined} settings
 * @param {string} key
 * @param {string} [fallback='']
 * @returns {string}
 */
export function getSettingStringSafe(settings, key, fallback = '') {
    if (!settings || !key)
        return fallback;
    if (!hasSettingKey(settings, key)) {
        const map = FALLBACK_STRING_STORE.get(settings);
        return map?.has(key) ? map.get(key) : fallback;
    }
    try {
        const val = settings.get_string(key);
        return typeof val === 'string' && val !== '' ? val : fallback;
    } catch (_e) {
        return fallback;
    }
}

/**
 * Writes a string (`s`) GSettings key safely, degrading gracefully to an in-memory
 * fallback map if the schema has not been recompiled yet.
 *
 * @param {Gio.Settings|null|undefined} settings
 * @param {string} key
 * @param {string} value
 * @returns {boolean} `true` if persisted to GSettings, `false` if stored in memory fallback
 */
export function setSettingStringSafe(settings, key, value) {
    if (!settings || !key)
        return false;
    const strVal = String(value ?? '');
    if (hasSettingKey(settings, key)) {
        try {
            settings.set_string(key, strVal);
            return true;
        } catch (_e) {
            // Fall through to in-memory fallback
        }
    }
    let map = FALLBACK_STRING_STORE.get(settings);
    if (!map) {
        map = new Map();
        FALLBACK_STRING_STORE.set(settings, map);
    }
    map.set(key, strVal);
    return false;
}

/**
 * Reads a `strv` (`as`) GSettings key safely, returning a copy of `fallback`
 * when the key is absent from an older compiled schema.
 *
 * @param {Gio.Settings|null|undefined} settings
 * @param {string} key
 * @param {ReadonlyArray<string>} [fallback=[]]
 * @returns {string[]}
 */
export function getSettingStrvSafe(settings, key, fallback = []) {
    if (!hasSettingKey(settings, key))
        return [...fallback];
    try {
        const val = settings.get_strv(key);
        return Array.isArray(val) ? val : [...fallback];
    } catch (_e) {
        return [...fallback];
    }
}

/**
 * Reads a boolean (`b`) GSettings key safely, returning `fallback` when the key
 * is absent from an older compiled schema.
 *
 * @param {Gio.Settings|null|undefined} settings
 * @param {string} key
 * @param {boolean} [fallback=false]
 * @returns {boolean}
 */
export function getSettingBoolSafe(settings, key, fallback = false) {
    if (!hasSettingKey(settings, key))
        return Boolean(fallback);
    try {
        return Boolean(settings.get_boolean(key));
    } catch (_e) {
        return Boolean(fallback);
    }
}

/**
 * Connects to `changed::${key}` only when `key` exists in `settings`'s compiled schema.
 * Returns `0` when the key is absent so callers can safely skip disconnecting `0`.
 *
 * @param {Gio.Settings|null|undefined} settings
 * @param {string} key
 * @param {Function} callback
 * @returns {number}
 */
export function connectSettingChangedSafe(settings, key, callback) {
    if (!hasSettingKey(settings, key))
        return 0;
    try {
        return settings.connect(`changed::${key}`, callback);
    } catch (_e) {
        return 0;
    }
}
