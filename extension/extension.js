import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const REFRESH_INTERVAL_SECONDS = 300; // 5 minutes
const BINARY_NAME = 'gnome-agent-status';

function resolveBinaryPath() {
    const homeDir = GLib.get_home_dir();
    const candidates = [
        GLib.build_filenamev([homeDir, '.local', 'bin', BINARY_NAME]),
        GLib.build_filenamev([homeDir, '.cargo', 'bin', BINARY_NAME]),
        `/usr/local/bin/${BINARY_NAME}`,
        `/usr/bin/${BINARY_NAME}`,
    ];

    for (const candidate of candidates) {
        if (GLib.file_test(candidate, GLib.FileTest.IS_EXECUTABLE))
            return candidate;
    }

    const fromPath = GLib.find_program_in_path(BINARY_NAME);
    return fromPath ?? BINARY_NAME;
}

function getConfigPath() {
    const configDir = GLib.get_user_config_dir();
    return GLib.build_filenamev([configDir, 'gnome-agent-status', 'config.toml']);
}

const AgentStatusIndicator = GObject.registerClass(
class AgentStatusIndicator extends PanelMenu.Button {
    _init() {
        super._init(0.0, 'Agent Usage Status', false);

        this._cancellable = null;
        this._timerId = null;
        this._isRefreshing = false;

        this._box = new St.BoxLayout({
            style_class: 'panel-status-menu-box agent-status-box',
        });

        this._label = new St.Label({
            text: 'Agents: …',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'agent-status-panel-label',
        });

        this._box.add_child(this._label);
        this.add_child(this._box);

        this._buildMenuSkeleton();
    }

    start() {
        this._refresh();
        this._timerId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT,
            REFRESH_INTERVAL_SECONDS,
            () => {
                this._refresh();
                return GLib.SOURCE_CONTINUE;
            }
        );
    }

    stop() {
        if (this._timerId !== null) {
            GLib.Source.remove(this._timerId);
            this._timerId = null;
        }
        if (this._cancellable !== null) {
            this._cancellable.cancel();
            this._cancellable = null;
        }
    }

    _buildMenuSkeleton() {
        this.menu.removeAll();

        this._agentsSection = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._agentsSection);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._updatedItem = new PopupMenu.PopupMenuItem('Updating…', {
            reactive: false,
            style_class: 'agent-status-meta-item',
        });
        this.menu.addMenuItem(this._updatedItem);

        const refreshItem = new PopupMenu.PopupMenuItem('Refresh Now');
        refreshItem.connect('activate', () => {
            this._refresh();
        });
        this.menu.addMenuItem(refreshItem);

        const configItem = new PopupMenu.PopupMenuItem('Open Config');
        configItem.connect('activate', () => {
            const cfgPath = getConfigPath();
            const uri = GLib.filename_to_uri(cfgPath, null);
            try {
                Gio.AppInfo.launch_default_for_uri(uri, null);
            } catch (e) {
                console.error(`[gnome-agent-status] Failed to open config: ${e.message}`);
            }
        });
        this.menu.addMenuItem(configItem);
    }

    _refresh() {
        if (this._isRefreshing)
            return;

        this._isRefreshing = true;
        if (this._cancellable !== null)
            this._cancellable.cancel();

        this._cancellable = new Gio.Cancellable();
        const binPath = resolveBinaryPath();

        let proc;
        try {
            proc = Gio.Subprocess.new(
                [binPath],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE
            );
        } catch (e) {
            this._isRefreshing = false;
            this._renderError(`Cannot run ${BINARY_NAME}: ${e.message}`);
            return;
        }

        proc.communicate_utf8_async(null, this._cancellable, (source, res) => {
            this._isRefreshing = false;
            try {
                const [, stdout, stderr] = source.communicate_utf8_finish(res);
                if (!stdout || stdout.trim() === '') {
                    const errMsg = stderr ? stderr.trim() : 'Empty output from binary';
                    this._renderError(errMsg);
                    return;
                }
                const payload = JSON.parse(stdout);
                this._renderPayload(payload);
            } catch (e) {
                if (e.matches && e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                    return;
                this._renderError(`Update error: ${e.message}`);
            }
        });
    }

    _renderPayload(payload) {
        this._label.set_text(payload.label || 'Agents: N/A');

        this._agentsSection.removeAll();
        const agents = Array.isArray(payload.agents) ? payload.agents : [];

        if (agents.length === 0) {
            const emptyItem = new PopupMenu.PopupMenuItem('No agents configured', {
                reactive: false,
            });
            this._agentsSection.addMenuItem(emptyItem);
        } else {
            for (const agent of agents) {
                let headerText;
                if (typeof agent.used_percent === 'number') {
                    headerText = `${agent.name} (${agent.agent_type}): ${agent.used_percent}% used · ${agent.remaining_percent}% left`;
                } else {
                    headerText = `${agent.name} (${agent.agent_type}): ERROR`;
                }

                const item = new PopupMenu.PopupMenuItem(headerText, {
                    reactive: false,
                    style_class: 'agent-status-item-header',
                });
                this._agentsSection.addMenuItem(item);

                const subLines = [];
                if (agent.reset_info)
                    subLines.push(`Reset: ${agent.reset_info}`);
                if (agent.detail && agent.detail !== agent.reset_info)
                    subLines.push(agent.detail);
                if (agent.warning)
                    subLines.push(`⚠ ${agent.warning}`);
                if (agent.error)
                    subLines.push(`✖ ${agent.error}`);

                for (const line of subLines) {
                    const subItem = new PopupMenu.PopupMenuItem(`  ${line}`, {
                        reactive: false,
                        style_class: 'agent-status-item-detail',
                    });
                    this._agentsSection.addMenuItem(subItem);
                }
            }
        }

        const now = GLib.DateTime.new_now_local();
        const timeStr = now ? now.format('%H:%M:%S') : '';
        this._updatedItem.label.set_text(`Last updated: ${timeStr} (every 5m)`);
    }

    _renderError(message) {
        this._label.set_text('Agents: ERR');
        this._agentsSection.removeAll();
        const errItem = new PopupMenu.PopupMenuItem(message, {
            reactive: false,
            style_class: 'agent-status-item-detail',
        });
        this._agentsSection.addMenuItem(errItem);
    }
});

export default class AgentStatusExtension extends Extension {
    enable() {
        this._indicator = new AgentStatusIndicator();
        Main.panel.addToStatusArea(this.uuid, this._indicator);
        this._indicator.start();
    }

    disable() {
        if (this._indicator) {
            this._indicator.stop();
            this._indicator.destroy();
            this._indicator = null;
        }
    }
}
