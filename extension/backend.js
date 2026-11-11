import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import {
    TERMINAL_CANDIDATES,
    buildTerminalArgv,
    normalizeStatusReport,
} from './format.js';

const BINARY_NAME = 'gnome-agent-status';
const MIN_POLL_SECONDS = 30;
const MONITOR_DEBOUNCE_MS = 60;
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
     *   onReport?: (report: object) => void,
     *   onError?: (message: string) => void,
     *   onLoadingChanged?: (isLoading: boolean) => void
     * }} params
     */
    constructor({settings, onReport, onError, onLoadingChanged}) {
        this._settings = settings;
        this._onReport = onReport ?? null;
        this._onError = onError ?? null;
        this._onLoadingChanged = onLoadingChanged ?? null;

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
        this._fileMonitor = null;
        this._fileMonitorSigId = 0;
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
        this._restartPollTimer();

        const syncReport = this._hydrateFromStateFileSync();
        void this._hydrateInitialState(syncReport);
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
     * Reads and normalizes `$XDG_CACHE_HOME/gnome-agent-status/state.json` synchronously
     * (`~0.1–0.2ms` from page cache) without touching subprocesses or network I/O.
     *
     * @returns {object|null}
     */
    _readStateReportSync() {
        if (this._destroyed)
            return null;

        try {
            if (!GLib.file_test(this._stateFilePath, GLib.FileTest.EXISTS))
                return null;

            const [ok, bytes] = GLib.file_get_contents(this._stateFilePath);
            if (!ok || !bytes || bytes.length === 0)
                return null;

            const rawText = UTF8_DECODER.decode(bytes);
            if (!rawText || rawText.trim() === '')
                return null;

            const parsed = JSON.parse(rawText);
            return normalizeStatusReport(parsed);
        } catch (e) {
            if (!this._destroyed)
                console.error(`[gnome-agent-status] Error hydrating state.json synchronously: ${e.message}`);
            return null;
        }
    }

    /**
     * Hydrates cached `state.json` synchronously during `enable()` so screen-unlock / startup
     * renders top-bar pills/labels on frame 0 with zero empty-frame flicker.
     *
     * @returns {object|null}
     */
    _hydrateFromStateFileSync() {
        const report = this._readStateReportSync();
        if (report)
            this._emitReport(report);
        return report;
    }

    async _hydrateInitialState(initialReport = null) {
        const loaded = initialReport ?? await this._readStateFile();
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
                this._emitReport(report);
                return report;
            }
        } catch (_e) {
            // Ignore cached fallback error; `collect(false)` runs next
        }
        return null;
    }

    async _readStateFile() {
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
            this._emitReport(report);
            return report;
        } catch (e) {
            if (!this._destroyed && !e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                console.error(`[gnome-agent-status] Error reading state.json: ${e.message}`);
            return null;
        } finally {
            this._releaseCancellable(cancellable);
        }
    }

    _emitReport(report) {
        if (this._destroyed)
            return;
        this._lastReport = report;
        this._lastError = null;
        this._onReport?.(report);
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
                const msg = res.stderr.trim() || 'Collector exited with an error';
                this._emitError(msg);
            }
            return null;
        } catch (e) {
            if (!this._destroyed && !e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                this._emitError(e.message || 'Collection failed');
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
                this._emitError(res.stderr.trim() || 'Failed to switch account');
            return null;
        } catch (e) {
            if (!this._destroyed && !e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                this._emitError(e.message || 'Failed to switch account');
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
            this._emitError('No supported terminal emulator found to run Sign in');
            return false;
        }

        // Resolve Debian/Ubuntu alternatives symlink (e.g. x-terminal-emulator -> ptyxis)
        if (chosenTerminal.endsWith('/x-terminal-emulator')) {
            let resolved = chosenTerminal;
            for (let depth = 0; depth < 8; depth++) {
                if (!GLib.file_test(resolved, GLib.FileTest.IS_SYMLINK))
                    break;
                try {
                    const linkTarget = GLib.file_read_link(resolved);
                    resolved = GLib.path_is_absolute(linkTarget)
                        ? linkTarget
                        : GLib.build_filenamev([GLib.path_get_dirname(resolved), linkTarget]);
                } catch (_e) {
                    break;
                }
            }
            chosenTerminal = resolved;
        }

        try {
            const argv = buildTerminalArgv(chosenTerminal, loginCommand);
            Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE);
            return true;
        } catch (e) {
            console.error(`[gnome-agent-status] Failed to launch terminal: ${e.message}`);
            this._emitError(`Failed to launch terminal: ${e.message}`);
            return false;
        }
    }
}
