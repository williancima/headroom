import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {ResetFlow, claimParameter, liveKeys} from './resets_logic.js';
import {resetsList} from './resets.js';
import {BreakdownOpen} from './breakdown_logic.js';
import {breakdownPanel} from './breakdown.js';
import {barParts, menuIds, metricLabel, roles} from './layout_logic.js';
import {Share} from './share.js';
import {priorityCopyVisible} from './share_logic.js';
import {choiceBar, clickable, membersPanel, meter, providerIcon, reorderable, tabTrack, tooltip, vbox, wrap} from './layout.js';

const APP_NAME = 'dev.headroom.Headroom';
const APP_PATH = '/dev/headroom/Headroom';
const OWN_NAME = 'dev.headroom.Extension';
const OWN_PATH = '/dev/headroom/Extension';
const OWN_IFACE = '<node><interface name="dev.headroom.Extension"><method name="ToggleMenu"/></interface></node>';
const REFRESH_SECONDS = 60;
const DEBOUNCE_MS = 500;
const APP_WAIT_SECONDS = 20;

function ago(iso) {
    if (!iso)
        return 'Not updated yet';
    const dt = GLib.DateTime.new_from_iso8601(iso, null);
    if (!dt)
        return 'Not updated yet';
    const s = Math.max(0, Math.floor((GLib.DateTime.new_now_utc().to_unix() - dt.to_unix())));
    if (s < 60)
        return 'Updated just now';
    if (s < 3600)
        return `Updated ${Math.floor(s / 60)} min ago`;
    if (s < 86400)
        return `Updated ${Math.floor(s / 3600)} h ago`;
    return `Updated ${Math.floor(s / 86400)} d ago`;
}

function findCli() {
    const p = GLib.find_program_in_path('headroom');
    if (p)
        return p;
    const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'headroom']);
    return GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE) ? local : null;
}

const HeadroomIndicator = GObject.registerClass(
class HeadroomIndicator extends PanelMenu.Button {
    constructor(dir) {
        super(0.5, 'Headroom');
        this._dir = dir;
        this._tab = 'priority';
        this._data = null;
        this._error = null;
        this._lastLogged = null;
        this._inFlight = false;
        this._rerun = false;
        this._cancellable = new Gio.Cancellable();
        this._waits = new Set();
        this._sources = new Set();
        this._monitors = new Map();
        this._debounce = 0;
        this._tick = 0;
        this._flow = new ResetFlow(() => GLib.uuid_string_random());
        this._breakdowns = new BreakdownOpen();
        this._members = new BreakdownOpen(); // unified rows showing their accounts
        this._menus = new BreakdownOpen();   // rows and titles showing their right-click choices
        this._signal = 0;
        this._share = null;
        this._toggleSignal = 0;
        this._restore = 0;

        this._bar = new St.BoxLayout({style_class: 'panel-status-menu-box'});
        this.add_child(this._bar);
        this._drawBar();

        this.menu.box.add_style_class_name('headroom-menu');
        this._buildMenu();
        this.menu.connect('open-state-changed', (_m, open) => {
            if (open) {
                this._refresh();
                this._tick = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, REFRESH_SECONDS, () => {
                    this._refresh();
                    return GLib.SOURCE_CONTINUE;
                });
            } else {
                this._removeTick();
            }
        });
    }

    start() {
        // The app tells how a claim ended; the extension only asked.
        this._signal = Gio.DBus.session.signal_subscribe(APP_NAME, 'dev.headroom.Headroom.Resets', 'ClaimDone', APP_PATH, null,
            Gio.DBusSignalFlags.NONE, (_c, _s, _p, _i, _n, params) => {
                const [requestId, message, settled] = params.deepUnpack();
                if (this._flow.finish(requestId, message, settled) !== null) {
                    this._render();
                    this._refresh();
                }
            });
        this._share = new Share((name, params, onFail) => this._command(name, false, params, onFail), msg => this._setError(msg));
        this._watch();
        this._refresh();
    }

    _removeTick() {
        if (this._tick) {
            GLib.source_remove(this._tick);
            this._tick = 0;
        }
    }

    _buildMenu() {
        // Plain sections, not reactive:false menu items: those take the theme's insensitive (dimmed) look.
        const switchSection = new PopupMenu.PopupMenuSection();
        const row = new St.BoxLayout({style_class: 'headroom-switch', x_expand: true});
        const tabs = new St.BoxLayout({x_expand: true});
        tabs.layout_manager.homogeneous = true;
        this._tabs = {};
        for (const [key, text] of [['priority', 'Priority'], ['usage', 'All Usage']]) {
            const b = new St.Button({label: text, style_class: 'headroom-tab', x_expand: true, can_focus: true});
            b.connect('clicked', () => {
                this._tab = key;
                this._track.select(b);
                this._render();
            });
            this._tabs[key] = b;
            tabs.add_child(b);
        }
        this._track = tabTrack(tabs, this._tabs[this._tab]);
        row.add_child(this._track);
        switchSection.actor.add_child(row);
        this.menu.addMenuItem(switchSection);

        this._list = vbox({style_class: 'headroom-list', x_expand: true});
        this._scroll = new St.ScrollView({
            style_class: 'headroom-scroll', overlay_scrollbars: false, x_expand: true,
            hscrollbar_policy: St.PolicyType.NEVER, vscrollbar_policy: St.PolicyType.AUTOMATIC,
        });
        this._scroll.set_child(this._list);
        const scrollSection = new PopupMenu.PopupMenuSection();
        scrollSection.actor.add_child(this._scroll);
        this.menu.addMenuItem(scrollSection);

        const footerSection = new PopupMenu.PopupMenuSection();
        const footer = new St.BoxLayout({style_class: 'headroom-footer', x_expand: true});
        this._updated = wrap(new St.Label({style_class: 'headroom-updated', y_align: Clutter.ActorAlign.CENTER, x_expand: true}));
        footer.add_child(this._updated);
        const hideTooltips = [];
        for (const [name, icon, action, dismiss] of [
            ['Copy Priority Image', 'edit-copy-symbolic', 'copy-priority', false],
            ['Refresh Now', 'view-refresh-symbolic', 'refresh', false],
            ['Accounts', 'system-users-symbolic', 'accounts', true],
            ['Settings', 'emblem-system-symbolic', 'settings', true],
        ]) {
            const button = new St.Button({
                style_class: 'headroom-icon-button',
                accessible_name: name,
                can_focus: true,
                y_align: Clutter.ActorAlign.CENTER,
                child: new St.Icon({icon_name: icon, icon_size: 16}),
            });
            hideTooltips.push(tooltip(button, name));
            if (action === 'copy-priority') {
                this._copyButton = button;
                button.connect('clicked', () => this._share?.request('priority'));
            } else {
                button.connect('clicked', () => this._command(action, dismiss));
            }
            footer.add_child(button);
        }
        footerSection.actor.add_child(footer);
        this.menu.addMenuItem(footerSection);
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (!open)
                hideTooltips.forEach(hide => hide());
        });
    }

    // --- data ---

    _watch() {
        for (const base of [GLib.get_user_state_dir(), GLib.get_user_config_dir()]) {
            const path = GLib.build_filenamev([base, 'headroom']);
            if (this._monitors.has(path))
                continue;
            try {
                const m = Gio.File.new_for_path(path).monitor_directory(Gio.FileMonitorFlags.NONE, this._cancellable);
                m.connect('changed', () => this._debounced());
                this._monitors.set(path, m);
            } catch (e) {
                console.error(`Headroom: cannot watch ${path}: ${e.message}`);
            }
        }
    }

    _debounced() {
        if (this._debounce)
            GLib.source_remove(this._debounce);
        this._debounce = GLib.timeout_add(GLib.PRIORITY_DEFAULT, DEBOUNCE_MS, () => {
            this._debounce = 0;
            this._refresh();
            return GLib.SOURCE_REMOVE;
        });
    }

    _refresh() {
        this._watch();
        if (this._inFlight) {
            this._rerun = true;
            return;
        }
        const cli = findCli();
        if (!cli) {
            this._setError('The headroom command was not found. Install it on your PATH or in ~/.local/bin (see docs/gnome-extension.md).');
            return;
        }
        this._inFlight = true;
        let proc;
        try {
            proc = new Gio.Subprocess({
                argv: [cli, 'panel'],
                flags: Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE,
            });
            proc.init(this._cancellable);
        } catch (e) {
            this._inFlight = false;
            this._setError(`Could not run ${cli}: ${e.message}`);
            return;
        }
        proc.communicate_utf8_async(null, this._cancellable, (p, res) => {
            try {
                const [, out, err] = p.communicate_utf8_finish(res);
                if (!p.get_successful()) {
                    this._setError(`headroom panel failed: ${(err || '').trim() || `exit ${p.get_exit_status()}`}`);
                } else {
                    try {
                        this._data = JSON.parse(out);
                        this._error = null;
                        this._lastLogged = null;
                        this._render();
                    } catch (e) {
                        this._setError(`headroom panel printed invalid JSON: ${e.message}`);
                    }
                }
            } catch (e) {
                if (e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                    return;
                this._setError(`headroom panel failed: ${e.message}`);
            }
            this._inFlight = false;
            if (this._rerun) {
                this._rerun = false;
                this._refresh();
            }
        });
    }

    _setError(msg) {
        this._error = msg;
        if (msg !== this._lastLogged) {
            this._lastLogged = msg;
            console.error(`Headroom: ${msg}`);
        }
        this._render();
    }

    // --- commands to the app ---

    // `params` are the action's GLib.Variant arguments (none for refresh and accounts); `onFail` runs when the call fails.
    _command(name, close, params = [], onFail = null) {
        if (close)
            this.menu.close();
        this._whenAppRuns(() => this._activate(name, params, onFail), () => {
            onFail?.();
            this._setError("Couldn't start Headroom. Run headroom-app in a terminal to see why.");
        });
    }

    // AUTO_START makes the bus start headroom-app (D-Bus activation); a click waits until it owns its name.
    _whenAppRuns(run, missing) {
        let timer = 0;
        let watch = 0;
        const finish = then => {
            if (!this._waits.delete(finish))
                return;
            if (timer)
                GLib.source_remove(timer);
            Gio.bus_unwatch_name(watch);
            then?.();
        };
        this._waits.add(finish);
        watch = Gio.bus_watch_name(Gio.BusType.SESSION, APP_NAME, Gio.BusNameWatcherFlags.AUTO_START, () => finish(run), null);
        timer = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, APP_WAIT_SECONDS, () => {
            timer = 0;
            finish(missing);
            return GLib.SOURCE_REMOVE;
        });
    }

    _activate(name, params, onFail) {
        Gio.DBus.session.call(APP_NAME, APP_PATH, 'org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', [name, params, {}]), null,
            Gio.DBusCallFlags.NONE, -1, this._cancellable, (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    if (e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        return;
                    onFail?.();
                    const gone = e.matches?.(Gio.DBusError, Gio.DBusError.SERVICE_UNKNOWN) ||
                        e.matches?.(Gio.DBusError, Gio.DBusError.NAME_HAS_NO_OWNER);
                    this._setError(gone
                        ? "Couldn't start Headroom. Run headroom-app in a terminal to see why."
                        : `Could not reach Headroom: ${e.message}`);
                }
            });
    }

    // --- drawing ---

    _claim(key, entry, requestId) {
        const param = GLib.Variant.new_string(claimParameter(key, entry.claim, requestId));
        this._command('claim-reset', false, [param], () => {
            this._flow.finish(requestId, "Couldn't reach Headroom. Try again.", false);
            this._render();
        });
    }

    _drawBar() {
        this._bar.destroy_all_children();
        for (const {icon, text} of barParts(this._data)) {
            if (icon) {
                this._bar.add_child(new St.Icon({
                    gicon: Gio.icon_new_for_string(this._dir.get_child(icon).get_path()),
                    style_class: 'system-status-icon',
                }));
            }
            if (text)
                this._bar.add_child(new St.Label({text, y_align: Clutter.ActorAlign.CENTER}));
        }
    }

    _render() {
        const scrolled = this._scroll.vadjustment?.value ?? 0;
        this._flow.reconcile(liveKeys(this._data?.usage));
        this._drawBar();
        for (const [k, b] of Object.entries(this._tabs)) {
            if (k === this._tab)
                b.add_style_class_name('headroom-tab-active');
            else
                b.remove_style_class_name('headroom-tab-active');
        }
        this._list.destroy_all_children();
        if (this._error) {
            this._list.add_child(wrap(new St.Label({text: this._error, style_class: 'headroom-error', x_expand: true})));
        }
        for (const g of this._data?.[this._tab] ?? []) {
            if (g.title)
                this._list.add_child(this._title(g));
            const rowRoles = roles(g);
            (g.rows ?? []).forEach((r, i) => this._list.add_child(this._card(r, g, rowRoles[i])));
        }
        this._copyButton.visible = priorityCopyVisible(this._tab, this._data?.priority);
        this._updated.set_text(ago(this._data?.updated_at));
        const monitor = Main.layoutManager.primaryMonitor;
        if (monitor)
            this._scroll.set_style(`max-height: ${Math.floor(monitor.height * 0.6)}px;`);
        // The rebuilt list starts at the top; put it back where the reader was.
        if (scrolled > 0) {
            if (this._restore)
                GLib.source_remove(this._restore);
            this._restore = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                this._restore = 0;
                this._scroll.vadjustment.value = scrolled;
                return GLib.SOURCE_REMOVE;
            });
        }
    }

    // Calls a layout action of the app; the app writes the files, which redraws the list.
    _layout(name, signature, args) {
        this._command(name, false, [new GLib.Variant(signature, args)]);
    }

    _toggleMenu(key) {
        this._menus.toggle(key);
        this._render();
    }

    // The group's title: drag it to move the provider; right click offers Unify Multiple Accounts.
    _title(g) {
        const box = vbox({x_expand: true});
        const title = new St.Label({text: g.title, style_class: 'headroom-group-title'});
        const menuKey = JSON.stringify(['title', g.kind ?? g.title]);
        if (g.unifiable)
            clickable(title, null, () => this._toggleMenu(menuKey));
        if (g.kind && this._tab === 'usage')
            reorderable(title, {type: 'provider', kind: g.kind}, g.title, (...a) => this._layout(...a));
        box.add_child(title);
        if (g.unifiable && this._menus.isOpen(menuKey)) {
            box.add_child(choiceBar([[`${g.unified ? '\u2713 ' : ''}Unify Multiple Accounts`, () => {
                this._menus.toggle(menuKey);
                this._layout('set-unified', '(sb)', [g.kind, !g.unified]);
                this._render();
            }]]));
        }
        return box;
    }

    _card(r, g, role) {
        const groupTitle = g.title;
        const card = vbox({style_class: 'headroom-card', x_expand: true});
        const top = new St.BoxLayout({x_expand: true});
        const open = r.card_id && r.resets?.length && this._tab === 'usage';
        const bKey = r.breakdown ? BreakdownOpen.key(r) : null;
        const mKey = r.members?.length ? JSON.stringify(['members', r.id]) : null;
        const ids = menuIds(r);
        const menuKey = ids ? JSON.stringify(['row', r.id]) : null;
        const toggle = (set, key) => () => {
            set.toggle(key);
            this._render();
        };
        let primary = null;
        let expanded = false;
        if (bKey !== null) {
            primary = toggle(this._breakdowns, bKey);
            expanded = this._breakdowns.isOpen(bKey);
        } else if (mKey !== null) {
            primary = toggle(this._members, mKey);
            expanded = this._members.isOpen(mKey);
        } else if (open) {
            primary = toggle(this._flow, r.card_id);
            expanded = this._flow.isOpen(r.card_id);
        }
        if (primary || menuKey)
            clickable(top, primary, menuKey ? () => this._toggleMenu(menuKey) : null);
        if (this._tab === 'usage' && role?.type === 'header') {
            reorderable(card, {type: 'account', cardId: role.cardId}, r.title, (...a) => this._layout(...a));
        } else if (this._tab === 'usage' && role?.type === 'metric' && role.cardId) {
            reorderable(card, {type: 'metric', cardId: role.cardId, label: metricLabel(r.id, role.cardId)}, r.title, (...a) => this._layout(...a));
        }
        if (r.kind)
            top.add_child(providerIcon(this._dir, r.kind));
        const texts = vbox({x_expand: true});
        texts.add_child(new St.Label({text: r.title, style_class: 'headroom-card-title'}));
        if (r.subtitle) {
            texts.add_child(wrap(new St.Label({text: r.subtitle, style_class: 'headroom-card-subtitle'})));
        }
        top.add_child(texts);
        if (r.value)
            top.add_child(new St.Label({text: r.value, style_class: 'headroom-card-value', y_align: Clutter.ActorAlign.START}));
        if (primary) {
            top.add_child(new St.Icon({
                // Same arrows as libadwaita's expander rows in the app windows.
                icon_name: expanded ? 'go-up-symbolic' : 'go-down-symbolic',
                icon_size: 12,
                opacity: 150,
                style_class: 'headroom-card-chevron',
                y_align: Clutter.ActorAlign.CENTER,
            }));
        }
        card.add_child(top);
        if (menuKey && this._menus.isOpen(menuKey)) {
            const done = (name, value) => () => {
                this._menus.toggle(menuKey);
                this._layout(name, '(asb)', [ids, value]);
                this._render();
            };
            card.add_child(choiceBar([[r.pinned ? 'Unpin' : 'Pin', done('set-pinned', !r.pinned)], ['Hide', done('set-hidden', true)]]));
        }
        if (mKey !== null && this._members.isOpen(mKey))
            card.add_child(membersPanel(r.members));
        if (open && this._flow.isOpen(r.card_id))
            card.add_child(resetsList(this._flow, r.card_id, r.resets, () => this._render(), (k, e, id) => this._claim(k, e, id)));
        if (bKey !== null && this._breakdowns.isOpen(bKey))
            card.add_child(breakdownPanel(r.breakdown, groupTitle, r.id ? () => this._share?.request('breakdown', r.id) : null));
        if (r.meter !== null && r.meter !== undefined)
            card.add_child(meter(r.meter));
        return card;
    }

    destroy() {
        this._cancellable.cancel();
        for (const finish of [...this._waits])
            finish(null);
        this._removeTick();
        if (this._signal)
            Gio.DBus.session.signal_unsubscribe(this._signal);
        this._signal = 0;
        this._share?.destroy();
        this._share = null;
        if (this._restore)
            GLib.source_remove(this._restore);
        this._restore = 0;
        if (this._debounce) {
            GLib.source_remove(this._debounce);
            this._debounce = 0;
        }
        for (const m of this._monitors.values())
            m.cancel();
        this._monitors.clear();
        super.destroy();
    }
});


export default class HeadroomExtension extends Extension {
    // `apt remove` deletes our folder, but GNOME keeps running the loaded code until the next login: watch the
    // folder and leave the top bar when metadata.json is gone, coming back if it is reinstalled.
    enable() {
        this._monitor = this.dir.monitor_directory(Gio.FileMonitorFlags.NONE, null);
        this._monitor.connect('changed', () => this._sync());
        this._sync();
    }

    _sync() {
        const installed = this.dir.get_child('metadata.json').query_exists(null);
        if (installed && !this._indicator)
            this._show();
        else if (!installed && this._indicator)
            this._hide();
    }

    _show() {
        this._indicator = new HeadroomIndicator(this.dir);
        // The app's Toggle Shortcut calls this; when the call fails (no extension, or an older one) the app opens its own panel.
        this._dbus = Gio.DBusExportedObject.wrapJSObject(OWN_IFACE, {ToggleMenu: () => this._indicator?.menu.toggle()});
        this._dbus.export(Gio.DBus.session, OWN_PATH);
        this._ownId = Gio.bus_own_name(Gio.BusType.SESSION, OWN_NAME, Gio.BusNameOwnerFlags.NONE,
            () => this._startApp(), null, null);
        Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');
        this._indicator.start();
    }

    // Started once our own name is taken: the app skips its tray icon when it sees that name.
    _startApp() {
        if (!this._indicator)
            return;
        Gio.DBus.session.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
            'StartServiceByName', new GLib.Variant('(su)', [APP_NAME, 0]), null,
            Gio.DBusCallFlags.NONE, -1, null, (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    console.error(`Headroom: could not start headroom-app: ${e.message}`);
                }
            });
    }

    disable() {
        this._monitor?.cancel();
        this._monitor = null;
        this._hide();
    }

    _hide() {
        this._indicator?.destroy();
        this._indicator = null;
        if (this._ownId) {
            Gio.bus_unown_name(this._ownId);
            this._ownId = 0;
        }
        this._dbus?.unexport();
        this._dbus = null;
    }
}
