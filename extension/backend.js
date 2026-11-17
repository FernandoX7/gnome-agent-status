import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import {_} from './compat.js';
import {
    TERMINAL_CANDIDATES,
    buildTerminalArgv,
    defaultCredentialWatchPaths,
    normalizeStatusReport,
} from './format.js';

const BINARY_NAME = 'gnome-agent-status';
const MIN_POLL_SECONDS = 30;
const MONITOR_DEBOUNCE_MS = 60;
const WAKE_NETWORK_DEBOUNCE_MS = 2500;
const CRED_MONITOR_DEBOUNCE_MS = 1500;
const LOGIN1_BUS_NAME = 'org.freedesktop.login1';
const LOGIN1_OBJECT_PATH = '/org/freedesktop/login1';
const LOGIN1_MANAGER_IFACE = 'org.freedesktop.login1.Manager';
const UTF8_DECODER = new TextDecoder('utf-8');

/**
 * Queries a Gio.File asynchronously on GIO's worker pool to check whether it is
 * an executable regular file without touching the compositor main thread.
 *
 * @param {string} filePath
 * @param {Gio.Cancellable|null} cancellable
 * @returns {Promise<boolean>}
 */
function checkExecutableAsync(filePath, cancellable) {
    if (!filePath)
        return Promise.resolve(false);

    const file = Gio.File.new_for_path(filePath);
    return new Promise(resolve => {
        file.query_info_async(
            'standard::type,access::can-execute',
            Gio.FileQueryInfoFlags.NONE,
            GLib.PRIORITY_DEFAULT,
            cancellable,
            (src, res) => {
                try {
                    const info = src.query_info_finish(res);
                    const isRegular = info.get_file_type() === Gio.FileType.REGULAR ||
                        info.get_file_type() === Gio.FileType.SYMBOLIC_LINK;
                    const canExec = info.get_attribute_boolean('access::can-execute');
                    resolve(Boolean(isRegular && canExec));
                } catch (_e) {
                    resolve(false);
                }
            }
        );
    });
}

/**
 * Queries a Gio.File asynchronously on GIO's worker pool to inspect its `Gio.FileType`
 * (`'dir'`, `'file'`, or `null` when absent) without blocking the compositor thread.
 *
 * @param {string} filePath
 * @param {Gio.Cancellable|null} cancellable
 * @returns {Promise<'dir'|'file'|null>}
 */
function queryPathKindAsync(filePath, cancellable) {
    if (!filePath)
        return Promise.resolve(null);

    const file = Gio.File.new_for_path(filePath);
    return new Promise(resolve => {
        file.query_info_async(
            'standard::type',
            Gio.FileQueryInfoFlags.NONE,
            GLib.PRIORITY_DEFAULT,
            cancellable,
            (src, res) => {
                try {
                    const info = src.query_info_finish(res);
                    const ft = info.get_file_type();
                    if (ft === Gio.FileType.DIRECTORY)
                        resolve('dir');
                    else
                        resolve('file');
                } catch (_e) {
                    resolve(null);
                }
            }
        );
    });
}

/**
 * Reads a symlink target asynchronously if `filePath` is a symbolic link, or `null` otherwise.
 *
 * @param {string} filePath
 * @param {Gio.Cancellable|null} cancellable
 * @returns {Promise<string|null>}
 */
function readSymlinkTargetAsync(filePath, cancellable) {
    if (!filePath)
        return Promise.resolve(null);

    const file = Gio.File.new_for_path(filePath);
    return new Promise(resolve => {
        file.query_info_async(
            'standard::type,standard::symlink-target',
            Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS,
            GLib.PRIORITY_DEFAULT,
            cancellable,
            (src, res) => {
                try {
                    const info = src.query_info_finish(res);
                    if (info.get_file_type() === Gio.FileType.SYMBOLIC_LINK)
                        resolve(info.get_symlink_target() || null);
                    else
                        resolve(null);
                } catch (_e) {
                    resolve(null);
                }
            }
        );
    });
}

/**
 * Checks asynchronously whether a regular file exists at `filePath`.
 *
 * @param {string} filePath
 * @param {Gio.Cancellable|null} cancellable
 * @returns {Promise<boolean>}
 */
function checkFileExistsAsync(filePath, cancellable) {
    if (!filePath)
        return Promise.resolve(false);

    const file = Gio.File.new_for_path(filePath);
    return new Promise(resolve => {
        file.query_info_async(
            'standard::type',
            Gio.FileQueryInfoFlags.NONE,
            GLib.PRIORITY_DEFAULT,
            cancellable,
            (src, res) => {
                try {
                    src.query_info_finish(res);
                    resolve(true);
                } catch (_e) {
                    resolve(false);
                }
            }
        );
    });
}

/**
 * Reads a UTF-8 file asynchronously via `Gio.File.load_contents_async`.
 *
 * @param {Gio.File} file
 * @param {Gio.Cancellable|null} cancellable
 * @returns {Promise<string|null>}
 */
function loadFileTextAsync(file, cancellable) {
    return new Promise((resolve, reject) => {
        file.load_contents_async(cancellable, (src, res) => {
            try {
                const [ok, bytes] = src.load_contents_finish(res);
                if (!ok || !bytes) {
                    resolve(null);
                    return;
                }
                const text = UTF8_DECODER.decode(bytes);
                resolve(text);
            } catch (e) {
                if (e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.NOT_FOUND)) {
                    resolve(null);
                    return;
                }
                reject(e);
            }
        });
    });
}

/**
 * Leak-free reactive client bridging GNOME Shell to `$XDG_CACHE_HOME/gnome-agent-status/state.json`
 * and the `gnome-agent-status` CLI.
 *
 * - Reads `state.json` in `<1ms` via `Gio.File.load_contents_async`
 * - Watches atomic `rename()` updates via `Gio.FileMonitor`
 * - Resolves binaries asynchronously off the main thread
 * - Tracks all in-flight `Gio.Subprocess` instances and calls `force_exit()` on `destroy()`
 * - Names all `GLib` timeout sources and guards every async continuation with `this._destroyed`
 */
export class BackendClient {
    /**
     * @param {{
     *   settings: Gio.Settings,
     *   onReport?: (report: object, meta?: {isInitialHydration?: boolean}) => void,
     *   onError?: (message: string) => void,
     *   onLoadingChanged?: (isLoading: boolean) => void,
     *   credentialWatchPaths?: string[]|null,
     *   systemBus?: Gio.DBusConnection|null
     * }} params
     */
    constructor({
        settings,
        onReport,
        onError,
        onLoadingChanged,
        credentialWatchPaths = null,
        systemBus = null,
    }) {
        this._settings = settings;
        this._onReport = onReport ?? null;
        this._onError = onError ?? null;
        this._onLoadingChanged = onLoadingChanged ?? null;
        this._customCredentialWatchPaths = Array.isArray(credentialWatchPaths)
            ? [...credentialWatchPaths]
            : null;
        this._injectedSystemBus = systemBus ?? null;

        this._destroyed = false;
        this._resolvedBinary = null;
        this._binaryResolvePromise = null;

        this._lastReport = null;
        this._lastError = null;
        this._isCollecting = false;
        this._queuedForceCollect = false;

        this._activeProcs = new Set();
        this._cancellables = new Set();

        this._pollTimerId = 0;
        this._monitorDebounceId = 0;
        this._wakeNetworkDebounceId = 0;
        this._credDebounceId = 0;
        this._credPendingForce = false;
        this._fileMonitor = null;
        this._fileMonitorSigId = 0;
        this._credMonitors = [];
        this._networkMonitor = null;
        this._networkMonitorSigId = 0;
        this._systemBus = null;
        this._sleepSubId = 0;
        this._settingsSigIds = [];

        this._stateFilePath = GLib.build_filenamev([
            GLib.get_user_cache_dir(),
            'gnome-agent-status',
            'state.json',
        ]);
        this._configFilePath = GLib.build_filenamev([
            GLib.get_user_config_dir(),
            'gnome-agent-status',
            'config.toml',
        ]);
    }

    get lastReport() {
        return this._lastReport;
    }

    get lastError() {
        return this._lastError;
    }

    get isCollecting() {
        return this._isCollecting;
    }

    getConfigPath() {
        return this._configFilePath;
    }

    getStatePath() {
        return this._stateFilePath;
    }

    setCallbacks({onReport, onError, onLoadingChanged}) {
        if (onReport !== undefined)
            this._onReport = onReport;
        if (onError !== undefined)
            this._onError = onError;
        if (onLoadingChanged !== undefined)
            this._onLoadingChanged = onLoadingChanged;
    }

    start() {
        if (this._destroyed)
            return;

        this._settingsSigIds.push(
            this._settings.connect('changed::binary-path', () => {
                this._resolvedBinary = null;
                this._binaryResolvePromise = null;
            }),
            this._settings.connect('changed::refresh-interval-seconds', () => {
                this._restartPollTimer();
            })
        );

        this._setupStateMonitor();
        this._setupNetworkWatcher();
        this._setupSleepWatcher();
        void this._setupCredentialMonitorsAsync();
        this._restartPollTimer();

        void this._hydrateInitialState();
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;

        for (const sigId of this._settingsSigIds)
            this._settings.disconnect(sigId);
        this._settingsSigIds = [];

        if (this._pollTimerId !== 0) {
            GLib.Source.remove(this._pollTimerId);
            this._pollTimerId = 0;
        }

        if (this._monitorDebounceId !== 0) {
            GLib.Source.remove(this._monitorDebounceId);
            this._monitorDebounceId = 0;
        }

        if (this._wakeNetworkDebounceId !== 0) {
            GLib.Source.remove(this._wakeNetworkDebounceId);
            this._wakeNetworkDebounceId = 0;
        }

        if (this._credDebounceId !== 0) {
            GLib.Source.remove(this._credDebounceId);
            this._credDebounceId = 0;
        }

        if (this._fileMonitor) {
            if (this._fileMonitorSigId !== 0) {
                this._fileMonitor.disconnect(this._fileMonitorSigId);
                this._fileMonitorSigId = 0;
            }
            try {
                this._fileMonitor.cancel();
            } catch (_e) {
                // Ignore cancellation cleanup errors
            }
            this._fileMonitor = null;
        }

        for (const entry of this._credMonitors) {
            if (entry.monitor && entry.sigId !== 0) {
                try {
                    entry.monitor.disconnect(entry.sigId);
                } catch (_e) {
                    // Ignore disconnect cleanup errors
                }
            }
            if (entry.monitor) {
                try {
                    entry.monitor.cancel();
                } catch (_e) {
                    // Ignore cancellation cleanup errors
                }
            }
        }
        this._credMonitors = [];

        if (this._networkMonitor) {
            if (this._networkMonitorSigId !== 0) {
                try {
                    this._networkMonitor.disconnect(this._networkMonitorSigId);
                } catch (_e) {
                    // Ignore disconnect cleanup errors
                }
                this._networkMonitorSigId = 0;
            }
            this._networkMonitor = null;
        }

        if (this._systemBus && this._sleepSubId !== 0) {
            try {
                this._systemBus.signal_unsubscribe(this._sleepSubId);
            } catch (_e) {
                // Ignore D-Bus unsubscribe cleanup errors
            }
        }
        this._sleepSubId = 0;
        this._systemBus = null;

        for (const cancellable of this._cancellables) {
            try {
                cancellable.cancel();
            } catch (_e) {
                // Ignore cancellation cleanup errors
            }
        }
        this._cancellables.clear();

        for (const proc of this._activeProcs) {
            try {
                proc.force_exit();
            } catch (_e) {
                // Ignore already-exited subprocess errors
            }
        }
        this._activeProcs.clear();
    }

    stop() {
        this.destroy();
    }

    _newCancellable() {
        const cancellable = new Gio.Cancellable();
        this._cancellables.add(cancellable);
        return cancellable;
    }

    _releaseCancellable(cancellable) {
        this._cancellables.delete(cancellable);
    }

    _getPollIntervalSeconds() {
        const raw = this._settings.get_int('refresh-interval-seconds');
        return Math.max(MIN_POLL_SECONDS, raw || 300);
    }

    _restartPollTimer() {
        if (this._pollTimerId !== 0) {
            GLib.Source.remove(this._pollTimerId);
            this._pollTimerId = 0;
        }
        if (this._destroyed)
            return;

        const intervalSec = this._getPollIntervalSeconds();
        this._pollTimerId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT,
            intervalSec,
            () => {
                if (this._destroyed) {
                    this._pollTimerId = 0;
                    return GLib.SOURCE_REMOVE;
                }
                void this.collect(false);
                return GLib.SOURCE_CONTINUE;
            }
        );
        GLib.Source.set_name_by_id(this._pollTimerId, '[gnome-agent-status] background-poll');
    }

    _setupStateMonitor() {
        if (this._destroyed)
            return;

        try {
            const stateFile = Gio.File.new_for_path(this._stateFilePath);
            this._fileMonitor = stateFile.monitor_file(Gio.FileMonitorFlags.NONE, null);
            this._fileMonitorSigId = this._fileMonitor.connect(
                'changed',
                (_monitor, _file, _otherFile, eventType) => {
                    if (this._destroyed)
                        return;
                    if (
                        eventType !== Gio.FileMonitorEvent.CHANGES_DONE_HINT &&
                        eventType !== Gio.FileMonitorEvent.CREATED &&
                        eventType !== Gio.FileMonitorEvent.CHANGED
                    ) {
                        return;
                    }
                    this._scheduleMonitorReload();
                }
            );
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to watch state.json: ${e.message}`);
        }
    }

    _scheduleMonitorReload() {
        if (this._destroyed || this._monitorDebounceId !== 0)
            return;

        this._monitorDebounceId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            MONITOR_DEBOUNCE_MS,
            () => {
                this._monitorDebounceId = 0;
                if (!this._destroyed)
                    void this._readStateFile();
                return GLib.SOURCE_REMOVE;
            }
        );
        GLib.Source.set_name_by_id(this._monitorDebounceId, '[gnome-agent-status] state-monitor-debounce');
    }

    /**
     * Subscribes to `Gio.NetworkMonitor.get_default()` `'network-changed'` and
     * triggers a `2500ms`-debounced `collect(false)` when connectivity returns.
     */
    _setupNetworkWatcher() {
        if (this._destroyed)
            return;

        try {
            const monitor = Gio.NetworkMonitor.get_default();
            if (!monitor)
                return;
            this._networkMonitor = monitor;
            this._networkMonitorSigId = monitor.connect(
                'network-changed',
                (mon, available) => {
                    if (this._destroyed)
                        return;
                    const isAvailable = typeof available === 'boolean'
                        ? available
                        : Boolean(mon.get_network_available?.());
                    if (isAvailable) {
                        this._scheduleWakeOrNetworkCollect();
                    } else if (this._wakeNetworkDebounceId !== 0) {
                        GLib.Source.remove(this._wakeNetworkDebounceId);
                        this._wakeNetworkDebounceId = 0;
                    }
                }
            );
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to attach Gio.NetworkMonitor: ${e.message}`);
        }
    }

    /**
     * Subscribes to `org.freedesktop.login1.Manager.PrepareForSleep` on the system
     * D-Bus so waking from suspend (`sleeping === false`) triggers a `2500ms`-debounced
     * `collect(false)` refresh once Wi-Fi / network interfaces settle.
     */
    _setupSleepWatcher() {
        if (this._destroyed)
            return;

        try {
            const bus = this._injectedSystemBus ?? Gio.DBus.system;
            if (!bus)
                return;
            this._systemBus = bus;
            this._sleepSubId = bus.signal_subscribe(
                LOGIN1_BUS_NAME,
                LOGIN1_MANAGER_IFACE,
                'PrepareForSleep',
                LOGIN1_OBJECT_PATH,
                null,
                Gio.DBusSignalFlags.NONE,
                (_conn, _sender, _path, _iface, _signal, params) => {
                    if (this._destroyed || !params)
                        return;
                    let sleeping = null;
                    try {
                        const unpacked = params.deepUnpack();
                        sleeping = Array.isArray(unpacked) ? unpacked[0] : unpacked;
                    } catch (_e) {
                        return;
                    }
                    if (sleeping === false) {
                        this._scheduleWakeOrNetworkCollect();
                    } else if (sleeping === true && this._wakeNetworkDebounceId !== 0) {
                        GLib.Source.remove(this._wakeNetworkDebounceId);
                        this._wakeNetworkDebounceId = 0;
                    }
                }
            );
        } catch (_e) {
            // System D-Bus may be unavailable in isolated test sessions
            this._systemBus = null;
            this._sleepSubId = 0;
        }
    }

    _scheduleWakeOrNetworkCollect() {
        if (this._destroyed)
            return;

        if (this._wakeNetworkDebounceId !== 0) {
            GLib.Source.remove(this._wakeNetworkDebounceId);
            this._wakeNetworkDebounceId = 0;
        }

        this._wakeNetworkDebounceId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            WAKE_NETWORK_DEBOUNCE_MS,
            () => {
                this._wakeNetworkDebounceId = 0;
                if (!this._destroyed)
                    void this.collect(false);
                return GLib.SOURCE_REMOVE;
            }
        );
        GLib.Source.set_name_by_id(
            this._wakeNetworkDebounceId,
            '[gnome-agent-status] wake-network-debounce'
        );
    }

    /**
     * Attaches `Gio.FileMonitor` filesystem event watchers to canonical provider
     * credential paths (`CHANGED` / `CHANGES_DONE_HINT` / `CREATED` / `DELETED`
     * events ONLY — credential file contents are NEVER read by the extension) so
     * completing `[Sign in]` or configuring API keys refreshes the popover within ~`1.5s`.
     */
    async _setupCredentialMonitorsAsync() {
        if (this._destroyed)
            return;

        const watchPaths = this._customCredentialWatchPaths ?? defaultCredentialWatchPaths(
            GLib.get_home_dir(),
            GLib.get_user_config_dir()
        );

        const watchedParentDirs = new Set();
        const cancellable = this._newCancellable();

        try {
            for (const credPath of watchPaths) {
                if (this._destroyed)
                    return;
                if (!credPath)
                    continue;

                const credKind = await queryPathKindAsync(credPath, cancellable);
                if (this._destroyed)
                    return;

                if (credKind === 'dir') {
                    this._attachCredentialDirectoryMonitor(credPath);
                    continue;
                }

                const isDirTarget = credPath.endsWith('/openrouter') || credPath.endsWith('/deepseek');
                const parentDir = GLib.path_get_dirname(credPath);
                const parentKind = parentDir ? await queryPathKindAsync(parentDir, cancellable) : null;
                if (this._destroyed)
                    return;

                if (isDirTarget) {
                    if (parentKind === 'dir')
                        this._attachDirEntryCreationMonitor(credPath);
                    continue;
                }

                if (credKind === 'file' || parentKind === 'dir') {
                    this._attachSingleCredentialFileMonitor(credPath);
                    continue;
                }

                // Only attach parent-creation watcher when the grandparent directory exists
                // (e.g. `~/.gemini` under `~`), skipping deeply nested uninstalled IDE paths
                // like `~/.config/Windsurf/User/globalStorage` when Windsurf is absent.
                const grandParent = parentDir ? GLib.path_get_dirname(parentDir) : null;
                const grandKind = grandParent ? await queryPathKindAsync(grandParent, cancellable) : null;
                if (this._destroyed)
                    return;

                if (
                    parentDir &&
                    grandKind === 'dir' &&
                    !watchedParentDirs.has(parentDir)
                ) {
                    watchedParentDirs.add(parentDir);
                    this._attachSingleCredentialFileMonitor(credPath);
                    this._attachParentDirCreationMonitor(parentDir, credPath);
                }
            }
        } finally {
            this._releaseCancellable(cancellable);
        }
    }

    _disconnectSingleCredMonitor(targetMonitor) {
        const idx = this._credMonitors.findIndex(e => e.monitor === targetMonitor);
        if (idx < 0)
            return;
        const [entry] = this._credMonitors.splice(idx, 1);
        if (entry.monitor && entry.sigId !== 0) {
            try {
                entry.monitor.disconnect(entry.sigId);
            } catch (_e) {}
        }
        if (entry.monitor) {
            try {
                entry.monitor.cancel();
            } catch (_e) {}
        }
    }

    _attachCredentialDirectoryMonitor(dirPath) {
        if (this._destroyed)
            return;

        try {
            const monitor = Gio.File.new_for_path(dirPath).monitor_directory(
                Gio.FileMonitorFlags.NONE,
                null
            );
            const sigId = monitor.connect(
                'changed',
                (_mon, _file, _otherFile, eventType) => {
                    if (this._destroyed)
                        return;
                    if (
                        eventType !== Gio.FileMonitorEvent.CHANGED &&
                        eventType !== Gio.FileMonitorEvent.CHANGES_DONE_HINT &&
                        eventType !== Gio.FileMonitorEvent.CREATED &&
                        eventType !== Gio.FileMonitorEvent.DELETED
                    ) {
                        return;
                    }
                    this._scheduleCredentialCollect(true);
                }
            );
            this._credMonitors.push({monitor, sigId, path: dirPath});
        } catch (_e) {
            // Non-fatal if filesystem blocks directory monitor attachment
        }
    }

    _attachDirEntryCreationMonitor(dirPath) {
        if (this._destroyed)
            return;

        try {
            const monitor = Gio.File.new_for_path(dirPath).monitor_file(
                Gio.FileMonitorFlags.NONE,
                null
            );
            let promoted = false;
            const sigId = monitor.connect(
                'changed',
                (_mon, _file, _otherFile, eventType) => {
                    if (this._destroyed || promoted)
                        return;
                    if (
                        eventType !== Gio.FileMonitorEvent.CREATED &&
                        eventType !== Gio.FileMonitorEvent.CHANGES_DONE_HINT
                    ) {
                        return;
                    }
                    promoted = true;
                    void (async () => {
                        const c = this._newCancellable();
                        try {
                            const kind = await queryPathKindAsync(dirPath, c);
                            if (this._destroyed)
                                return;
                            if (kind === 'dir') {
                                this._disconnectSingleCredMonitor(monitor);
                                this._attachCredentialDirectoryMonitor(dirPath);
                                this._scheduleCredentialCollect(true);
                            } else {
                                promoted = false;
                            }
                        } finally {
                            this._releaseCancellable(c);
                        }
                    })();
                }
            );
            this._credMonitors.push({monitor, sigId, path: dirPath});
        } catch (_e) {
            // Non-fatal fallback
        }
    }

    _attachSingleCredentialFileMonitor(credPath) {
        if (this._destroyed)
            return;

        // IDE SQLite state databases (`.vscdb`) mutate continuously during normal editing;
        // trigger TTL-aware non-force collection so active IDE editing never forces 30s polls.
        const forceCollect = !credPath.endsWith('.vscdb');

        try {
            const monitor = Gio.File.new_for_path(credPath).monitor_file(
                Gio.FileMonitorFlags.NONE,
                null
            );
            const sigId = monitor.connect(
                'changed',
                (_mon, _file, _otherFile, eventType) => {
                    if (this._destroyed)
                        return;
                    if (
                        eventType !== Gio.FileMonitorEvent.CHANGED &&
                        eventType !== Gio.FileMonitorEvent.CHANGES_DONE_HINT &&
                        eventType !== Gio.FileMonitorEvent.CREATED &&
                        eventType !== Gio.FileMonitorEvent.DELETED
                    ) {
                        return;
                    }
                    this._scheduleCredentialCollect(forceCollect);
                }
            );
            this._credMonitors.push({monitor, sigId, path: credPath});
        } catch (_e) {
            // Non-fatal if filesystem blocks monitor attachment on a candidate path
        }
    }

    _attachParentDirCreationMonitor(parentDir, credPath) {
        if (this._destroyed)
            return;

        try {
            const monitor = Gio.File.new_for_path(parentDir).monitor_file(
                Gio.FileMonitorFlags.NONE,
                null
            );
            let promoted = false;
            const sigId = monitor.connect(
                'changed',
                (_mon, _file, _otherFile, eventType) => {
                    if (this._destroyed || promoted)
                        return;
                    if (
                        eventType !== Gio.FileMonitorEvent.CREATED &&
                        eventType !== Gio.FileMonitorEvent.CHANGES_DONE_HINT
                    ) {
                        return;
                    }
                    promoted = true;
                    void (async () => {
                        const c = this._newCancellable();
                        try {
                            const pKind = await queryPathKindAsync(parentDir, c);
                            if (this._destroyed)
                                return;
                            if (pKind === 'dir') {
                                this._disconnectSingleCredMonitor(monitor);
                                this._attachSingleCredentialFileMonitor(credPath);
                                const cKind = await queryPathKindAsync(credPath, c);
                                if (!this._destroyed && cKind === 'file')
                                    this._scheduleCredentialCollect(!credPath.endsWith('.vscdb'));
                            } else {
                                promoted = false;
                            }
                        } finally {
                            this._releaseCancellable(c);
                        }
                    })();
                }
            );
            this._credMonitors.push({monitor, sigId, path: parentDir});
        } catch (_e) {
            // Non-fatal fallback
        }
    }

    _scheduleCredentialCollect(force = true) {
        if (this._destroyed)
            return;

        if (force)
            this._credPendingForce = true;

        if (this._credDebounceId !== 0) {
            GLib.Source.remove(this._credDebounceId);
            this._credDebounceId = 0;
        }

        this._credDebounceId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            CRED_MONITOR_DEBOUNCE_MS,
            () => {
                this._credDebounceId = 0;
                const useForce = this._credPendingForce;
                this._credPendingForce = false;
                if (!this._destroyed)
                    void this.collect(useForce);
                return GLib.SOURCE_REMOVE;
            }
        );
        GLib.Source.set_name_by_id(
            this._credDebounceId,
            '[gnome-agent-status] cred-monitor-debounce'
        );
    }

    async _hydrateInitialState(initialReport = null) {
        const loaded = initialReport ?? await this._readStateFile({isInitialHydration: true});
        if (this._destroyed)
            return;

        const nowEpoch = Math.floor(Date.now() / 1000);
        const intervalSec = this._getPollIntervalSeconds();
        const age = loaded ? nowEpoch - (loaded.updated_at_epoch || 0) : Infinity;

        if (!loaded) {
            await this.readCached();
            if (!this._destroyed)
                void this.collect(false);
        } else if (age >= intervalSec) {
            void this.collect(false);
        }
    }

    /**
     * Invokes `gnome-agent-status cached` (`<3ms`, zero network I/O) when `state.json`
     * has not been written yet so the UI hydrates immediately before a full `collect`.
     *
     * @returns {Promise<object|null>}
     */
    async readCached() {
        if (this._destroyed)
            return null;

        try {
            const res = await this._execCliAsync(['cached']);
            if (this._destroyed || !res)
                return null;

            const trimmed = res.stdout.trim();
            if (trimmed !== '') {
                const parsed = JSON.parse(trimmed);
                const report = normalizeStatusReport(parsed);
                this._emitReport(report, {isInitialHydration: true});
                return report;
            }
        } catch (_e) {
            // Ignore cached fallback error; `collect(false)` runs next
        }
        return null;
    }

    async _readStateFile({isInitialHydration = false} = {}) {
        if (this._destroyed)
            return null;

        const cancellable = this._newCancellable();
        try {
            const stateFile = Gio.File.new_for_path(this._stateFilePath);
            const rawText = await loadFileTextAsync(stateFile, cancellable);
            if (this._destroyed || !rawText || rawText.trim() === '')
                return null;

            const parsed = JSON.parse(rawText);
            const report = normalizeStatusReport(parsed);
            this._emitReport(report, {isInitialHydration});
            return report;
        } catch (e) {
            if (!this._destroyed && !e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                console.error(`[gnome-agent-status] Error reading state.json: ${e.message}`);
            return null;
        } finally {
            this._releaseCancellable(cancellable);
        }
    }

    _emitReport(report, meta = {isInitialHydration: false}) {
        if (this._destroyed)
            return;
        this._lastReport = report;
        this._lastError = null;
        this._onReport?.(report, meta);
    }

    _emitError(message) {
        if (this._destroyed)
            return;
        this._lastError = message;
        this._onError?.(message);
    }

    _setCollecting(active) {
        if (this._isCollecting === active)
            return;
        this._isCollecting = active;
        if (!this._destroyed)
            this._onLoadingChanged?.(active);
    }

    /**
     * Resolves the `gnome-agent-status` executable path asynchronously on GIO's worker pool.
     *
     * @returns {Promise<string>}
     */
    async resolveBinaryPathAsync() {
        if (this._resolvedBinary)
            return this._resolvedBinary;
        if (this._binaryResolvePromise)
            return this._binaryResolvePromise;

        this._binaryResolvePromise = (async () => {
            const cancellable = this._newCancellable();
            try {
                const custom = this._settings.get_string('binary-path').trim();
                if (custom !== '') {
                    const expanded = custom.startsWith('~/')
                        ? GLib.build_filenamev([GLib.get_home_dir(), custom.slice(2)])
                        : custom;
                    if (await checkExecutableAsync(expanded, cancellable)) {
                        this._resolvedBinary = expanded;
                        return expanded;
                    }
                    // Still respect explicit user override so any clear error message surfaces
                    this._resolvedBinary = expanded;
                    return expanded;
                }

                const homeDir = GLib.get_home_dir();
                const candidates = [
                    GLib.build_filenamev([homeDir, '.local', 'bin', BINARY_NAME]),
                    GLib.build_filenamev([homeDir, '.cargo', 'bin', BINARY_NAME]),
                    `/usr/local/bin/${BINARY_NAME}`,
                    `/usr/bin/${BINARY_NAME}`,
                ];

                const pathEnv = GLib.getenv('PATH') || '';
                for (const dir of pathEnv.split(':')) {
                    if (!dir)
                        continue;
                    const candidate = GLib.build_filenamev([dir, BINARY_NAME]);
                    if (!candidates.includes(candidate))
                        candidates.push(candidate);
                }

                for (const candidate of candidates) {
                    if (this._destroyed)
                        return BINARY_NAME;
                    if (await checkExecutableAsync(candidate, cancellable)) {
                        this._resolvedBinary = candidate;
                        return candidate;
                    }
                }

                this._resolvedBinary = BINARY_NAME;
                return BINARY_NAME;
            } finally {
                this._releaseCancellable(cancellable);
                this._binaryResolvePromise = null;
            }
        })();

        return this._binaryResolvePromise;
    }

    /**
     * Spawns `gnome-agent-status` with `args`, tracking the `Gio.Subprocess` for `force_exit()`
     * on `disable()` and returning `{ stdout, stderr, ok }`.
     *
     * @param {string[]} args
     * @returns {Promise<{stdout: string, stderr: string, ok: boolean}|null>}
     */
    async _execCliAsync(args) {
        if (this._destroyed)
            return null;

        const binPath = await this.resolveBinaryPathAsync();
        if (this._destroyed)
            return null;

        const cancellable = this._newCancellable();
        let proc = null;

        try {
            proc = Gio.Subprocess.new(
                [binPath, ...args],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE
            );
        } catch (e) {
            this._releaseCancellable(cancellable);
            this._resolvedBinary = null;
            throw new Error(`Cannot execute ${binPath}: ${e.message}`);
        }

        this._activeProcs.add(proc);

        try {
            const [stdout, stderr] = await new Promise((resolve, reject) => {
                proc.communicate_utf8_async(null, cancellable, (src, res) => {
                    try {
                        const [, out, err] = src.communicate_utf8_finish(res);
                        resolve([out ?? '', err ?? '']);
                    } catch (err) {
                        reject(err);
                    }
                });
            });

            if (this._destroyed)
                return null;

            const ok = proc.get_successful();
            return {stdout, stderr, ok};
        } finally {
            this._activeProcs.delete(proc);
            this._releaseCancellable(cancellable);
        }
    }

    /**
     * Triggers an asynchronous `gnome-agent-status collect [--force]` run.
     * Coalesces concurrent requests safely without dropping a manual `--force` request.
     *
     * @param {boolean} [force=false]
     * @returns {Promise<object|null>}
     */
    async collect(force = false) {
        if (this._destroyed)
            return null;

        if (this._isCollecting) {
            if (force)
                this._queuedForceCollect = true;
            return this._lastReport;
        }

        this._setCollecting(true);
        try {
            const args = force ? ['collect', '--force'] : ['collect'];
            const res = await this._execCliAsync(args);
            if (this._destroyed || !res)
                return null;

            const trimmed = res.stdout.trim();
            if (trimmed !== '') {
                const parsed = JSON.parse(trimmed);
                const report = normalizeStatusReport(parsed);
                this._emitReport(report);
                return report;
            }

            if (!res.ok) {
                const msg = res.stderr.trim() || _('Collector exited with an error');
                this._emitError(msg);
            }
            return null;
        } catch (e) {
            if (!this._destroyed && !e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                this._emitError(e.message || _('Collection failed'));
            return null;
        } finally {
            this._setCollecting(false);
            if (!this._destroyed && this._queuedForceCollect) {
                this._queuedForceCollect = false;
                void this.collect(true);
            }
        }
    }

    /**
     * Switches the pinned active account for `providerId` (`account select <providerId> <accountId>`)
     * and updates UI state immediately from the returned `StatusReport` v2 JSON.
     *
     * @param {string} providerId
     * @param {string} accountId
     * @returns {Promise<object|null>}
     */
    async selectAccount(providerId, accountId) {
        if (this._destroyed || !providerId || !accountId)
            return null;

        try {
            const res = await this._execCliAsync([
                'account',
                'select',
                String(providerId),
                String(accountId),
            ]);
            if (this._destroyed || !res)
                return null;

            const trimmed = res.stdout.trim();
            if (trimmed !== '') {
                const parsed = JSON.parse(trimmed);
                const report = normalizeStatusReport(parsed);
                this._emitReport(report);
                return report;
            }

            if (!res.ok)
                this._emitError(res.stderr.trim() || _('Failed to switch account'));
            return null;
        } catch (e) {
            if (!this._destroyed && !e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                this._emitError(e.message || _('Failed to switch account'));
            return null;
        }
    }

    /**
     * Ensures `$XDG_CONFIG_HOME/gnome-agent-status/config.toml` exists on disk
     * (running `gnome-agent-status --init-config` if absent) before returning its path.
     *
     * @returns {Promise<string>}
     */
    async ensureConfigInitialized() {
        const configPath = this._configFilePath;
        if (this._destroyed)
            return configPath;

        const cancellable = this._newCancellable();
        let exists = false;
        try {
            exists = await checkFileExistsAsync(configPath, cancellable);
        } finally {
            this._releaseCancellable(cancellable);
        }

        if (!exists && !this._destroyed) {
            try {
                await this._execCliAsync(['--init-config']);
            } catch (e) {
                console.error(`[gnome-agent-status] --init-config failed: ${e.message}`);
            }
        }
        return configPath;
    }

    /**
     * Resolves an installed terminal emulator off-thread and spawns `loginCommand`
     * in a new detached terminal window for `[Sign in]`.
     *
     * @param {string} loginCommand
     * @returns {Promise<boolean>}
     */
    async launchLoginTerminal(loginCommand) {
        if (this._destroyed)
            return false;

        const cancellable = this._newCancellable();
        let chosenTerminal = null;
        try {
            const rawDirs = (GLib.getenv('PATH') || '').split(':').filter(Boolean);
            const fallbackDirs = [
                '/usr/local/bin',
                '/usr/bin',
                '/bin',
                GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin']),
            ];
            const pathDirs = [...new Set([...rawDirs, ...fallbackDirs])];

            for (const termName of TERMINAL_CANDIDATES) {
                for (const dir of pathDirs) {
                    const candidate = GLib.build_filenamev([dir, termName]);
                    if (await checkExecutableAsync(candidate, cancellable)) {
                        chosenTerminal = candidate;
                        break;
                    }
                }
                if (chosenTerminal || this._destroyed)
                    break;
            }
        } finally {
            this._releaseCancellable(cancellable);
        }

        if (this._destroyed || !chosenTerminal) {
            this._emitError(_('No supported terminal emulator found to run Sign in'));
            return false;
        }

        const linkCancellable = this._newCancellable();
        let pretokenizedArgv = null;
        try {
            // Resolve Debian/Ubuntu alternatives symlink (e.g. x-terminal-emulator -> ptyxis) off-thread
            if (chosenTerminal.endsWith('/x-terminal-emulator')) {
                let resolved = chosenTerminal;
                for (let depth = 0; depth < 8; depth++) {
                    const linkTarget = await readSymlinkTargetAsync(resolved, linkCancellable);
                    if (!linkTarget || this._destroyed)
                        break;
                    resolved = GLib.path_is_absolute(linkTarget)
                        ? linkTarget
                        : GLib.build_filenamev([GLib.path_get_dirname(resolved), linkTarget]);
                }
                chosenTerminal = resolved;
            }

            const homeDir = GLib.get_home_dir();
            const rawTrimmed = String(loginCommand ?? '').replace(/\0/g, '').trim();
            if (rawTrimmed !== '') {
                const expanded = rawTrimmed === '~'
                    ? homeDir
                    : rawTrimmed.startsWith('~/')
                        ? GLib.build_filenamev([homeDir, rawTrimmed.slice(2)])
                        : rawTrimmed;
                if ((await queryPathKindAsync(expanded, linkCancellable)) === 'file')
                    pretokenizedArgv = [expanded];
            }
        } finally {
            this._releaseCancellable(linkCancellable);
        }

        if (this._destroyed)
            return false;

        try {
            const homeDir = GLib.get_home_dir();
            const argv = buildTerminalArgv(chosenTerminal, loginCommand, {
                quoteFn: s => GLib.shell_quote(String(s ?? '').replace(/\0/g, '')),
                homeDir,
                pretokenizedArgv,
            });
            const proc = Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE);
            proc.wait_async(null, (src, res) => {
                try {
                    src.wait_finish(res);
                } catch (_e) {}
            });
            return true;
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to launch terminal: ${e.message}`);
            this._emitError(_('Failed to launch terminal: %s').replace('%s', e.message));
            return false;
        }
    }
}
